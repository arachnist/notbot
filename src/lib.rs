mod spaceapi;

use anyhow::{Context, anyhow};
use serde::{Deserialize, Serialize};

use matrix_sdk::{
    matrix_auth::MatrixSession,
    config::SyncSettings,
    Client,
    Error,
    LoopCtrl,
    Room,
    ruma::events::room::{
        message::{MessageType, OriginalSyncRoomMessageEvent},
        member::StrippedRoomMemberEvent,
    },
};

use std::{
    fs,
    path::Path,
};

use tokio::time::{sleep, Duration};

/// The full session to persist.
#[derive(Debug, Serialize, Deserialize)]
struct Session {
    /// The Matrix user session.
    user_session: MatrixSession,

    /// The latest sync token.
    ///
    /// It is only needed to persist it when using `Client::sync_once()` and we
    /// want to make our syncs faster by not receiving all the initial sync
    /// again.
    #[serde(skip_serializing_if = "Option::is_none")]
    sync_token: Option<String>,
}

#[derive(Deserialize)]
pub struct Config {
    pub homeserver: String,
    pub user_id: String,
    pub password: String,
    pub data_dir: String,
    pub device_id: String,
}

impl Config {
    pub fn from_path(path: Option<String>) ->anyhow::Result<Self, anyhow::Error> {
        let config_path = match path {
            Some(s) => s,
            None => return Err(anyhow!("configuration path not provided")),
        };

        let config_content = fs::read_to_string(&config_path)
            .context("couldn't read config file")?;
        let config: Config = toml::from_str(&config_content)
            .context("couldn't parse config file")?;

        tracing::info!("using config: {config_path}");
        Ok(config)
    }
}

pub async fn run(config: Config) ->anyhow::Result<()> {
    let data_dir = Path::new(&config.data_dir);
    let session_file = data_dir.join("session.json");
    let (client, initial_sync_token) = if session_file.exists() {
        tracing::info!("previous session found, attempting restore");
        restore_session(config).await?
    } else {
        tracing::info!("no previous session found, attempting login");
        (login(config).await?, None)
    };

    let mut sync_settings = SyncSettings::default();
    if let Some(sync_token) = initial_sync_token {
        sync_settings = sync_settings.token(sync_token);
    }

    tracing::info!("performing initial sync to ignore old messages...");
    client.sync_once(sync_settings).await.unwrap();

    client.add_event_handler(on_room_message);
    client.add_event_handler(autojoin_on_invites);

    tracing::info!("finished initializing");
    client
        .sync_with_result_callback(SyncSettings::default(), |sync_result| {
            let session_path = session_file.clone();
            async move {
            let response = sync_result?;

            persist_sync_token(&session_path, response.next_batch)
                .await
                .map_err(|err| Error::UnknownError(err.into()))?;
            Ok(LoopCtrl::Continue)
        }})
        .await?;
    Ok(())
}

async fn persist_sync_token(session_file: &Path, sync_token: String) -> anyhow::Result<()> {
    let serialized_session = fs::read_to_string(session_file)?;
    let mut session: Session = serde_json::from_str(&serialized_session)?;

    session.sync_token = Some(sync_token);
    let serialized_session = serde_json::to_string(&session)?;
    fs::write(session_file, serialized_session)?;

    Ok(())
}

async fn restore_session(config: Config) ->anyhow::Result<(Client, Option<String>)> {
    let data_dir = Path::new(&config.data_dir);
    let session_file = data_dir.join("session.json");
    let db_path = data_dir.join("store.db");

    let serialized_session = fs::read_to_string(session_file)?;
    tracing::info!("deserializing session");
    let session: Session = serde_json::from_str(&serialized_session)?;

    tracing::info!("building client");
    let client = Client::builder()
        .homeserver_url(config.homeserver)
        .sqlite_store(db_path, None)
        .build()
        .await?;


    tracing::info!("restoring session");
    client.restore_session(session.user_session).await?;

    Ok((client, session.sync_token))
}

async fn login(config: Config) -> anyhow::Result<Client> {
    let data_dir = Path::new(&config.data_dir);
    let session_file = data_dir.join("session.json");
    let db_path = data_dir.join("store.db");

    tracing::info!("building client");
    let client = Client::builder()
        .homeserver_url(config.homeserver)
        .sqlite_store(db_path, None)
        .build()
        .await?;
    let auth = client.matrix_auth();
    tracing::info!("logging in");
    auth
        .login_username(&config.user_id, &config.password)
        .initial_device_display_name(&config.device_id)
        .await?;
    let user_session = auth.session().expect("A logged-in client should have a session");
    tracing::debug!("serializing session");
    let serialized_session = serde_json::to_string(&Session{user_session, sync_token: None})?;
    tracing::debug!("storing session");
    fs::write(session_file, serialized_session)?;

    Ok(client)
}

///// handlers copypasted from examples
/// Handle room messages.
async fn on_room_message(event: OriginalSyncRoomMessageEvent, room: Room) {
    // We only want to log text messages in joined rooms.
    // if room.state() != RoomState::Joined {
    //    return;
    // }
    let MessageType::Text(text_content) = &event.content.msgtype else { return };

    let room_name = match room.compute_display_name().await {
        Ok(room_name) => room_name.to_string(),
        Err(error) => {
            tracing::info!("Error getting room display name: {error}");
            // Let's fallback to the room ID.
            room.room_id().to_string()
        }
    };

    tracing::info!("[{room_name}] {}: {}", event.sender, text_content.body)
}

async fn autojoin_on_invites(
    room_member: StrippedRoomMemberEvent,
    client: Client,
    room: Room,
) {
    if room_member.state_key != client.user_id().unwrap() {
        return;
    }

    tokio::spawn(async move {
        tracing::info!("Autojoining room {}", room.room_id());
        let mut delay = 2;

        while let Err(err) = room.join().await {
            // retry autojoin due to synapse sending invites, before the
            // invited user can join for more information see
            // https://github.com/matrix-org/synapse/issues/4345
            tracing::error!("Failed to join room {} ({err:?}), retrying in {delay}s", room.room_id());

            sleep(Duration::from_secs(delay)).await;
            delay *= 2;

            if delay > 3600 {
                tracing::error!("Can't join room {} ({err:?})", room.room_id());
                break;
            }
        }
        tracing::info!("Successfully joined room {}", room.room_id());
    });
}
