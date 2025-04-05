mod autojoiner;
mod kasownik;
mod spaceapi;
mod wolfram;

use anyhow::{anyhow, Context};
use tracing::{debug, info, trace};

use serde::{de, Deserialize, Serialize};

use matrix_sdk::{
    config::SyncSettings,
    matrix_auth::MatrixSession,
    ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent},
    Client, Error, LoopCtrl, Room,
};

use reqwest::Client as RClient;

use std::{fs, future::Future, path::Path, pin::Pin};
use toml::Table;

use linkme::distributed_slice;

/// Modules registry
#[distributed_slice]
pub static MODULES: [fn(&Client, &Config)];

#[distributed_slice]
pub static ASYNC_MODULES: [fn(&Client, &Config) -> Pin<Box<dyn Future<Output = ()>>>];

/// The full session to persist.
#[derive(Debug, Serialize, Deserialize)]
struct Session {
    /// The Matrix user session.
    user_session: MatrixSession,

    /// The latest sync token.
    #[serde(skip_serializing_if = "Option::is_none")]
    sync_token: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Config {
    pub homeserver: String,
    pub user_id: String,
    pub password: String,
    pub data_dir: String,
    pub device_id: String,
    pub module: Table,
}

impl Config {
    pub fn from_path(path: Option<String>) -> anyhow::Result<Self, anyhow::Error> {
        let config_path = match path {
            Some(s) => s,
            None => return Err(anyhow!("configuration path not provided")),
        };

        let config_content =
            fs::read_to_string(&config_path).context("couldn't read config file")?;
        let config: Config =
            toml::from_str(&config_content).context("couldn't parse config file")?;

        info!("using config: {config_path}");
        Ok(config)
    }
}

pub async fn run(config: Config) -> anyhow::Result<()> {
    let data_dir = Path::new(&config.data_dir);
    let session_file = data_dir.join("session.json");
    let (client, initial_sync_token) = if session_file.exists() {
        info!("previous session found, attempting restore");
        restore_session(config.clone()).await?
    } else {
        info!("no previous session found, attempting login");
        (login(config.clone()).await?, None)
    };

    let mut sync_settings = SyncSettings::default().full_state(false);
    if let Some(sync_token) = initial_sync_token {
        debug!("initial sync token: {:#?}", &sync_token);
        sync_settings = sync_settings.token(sync_token);
    }

    debug!("adding config as extra context for callbacks");
    client.add_event_handler_context(config.clone());

    info!("performing initial sync");
    client.sync_once(SyncSettings::default()).await.unwrap();

    client.add_event_handler(on_room_message);

    for initializer in MODULES {
        initializer(&client, &config);
    }

    info!("finished initializing");
    client
        .sync_with_result_callback(sync_settings, |sync_result| {
            let session_path = session_file.clone();
            async move {
                let response = sync_result?;

                trace!("sync response: {:#?}", &response);

                persist_sync_token(&session_path, response.next_batch)
                    .await
                    .map_err(|err| Error::UnknownError(err.into()))?;
                Ok(LoopCtrl::Continue)
            }
        })
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

async fn restore_session(config: Config) -> anyhow::Result<(Client, Option<String>)> {
    let data_dir = Path::new(&config.data_dir);
    let session_file = data_dir.join("session.json");
    let db_path = data_dir.join("store.db");

    let serialized_session = fs::read_to_string(session_file)?;
    info!("deserializing session");
    let session: Session = serde_json::from_str(&serialized_session)?;

    info!("building client");
    let client = Client::builder()
        .homeserver_url(config.homeserver)
        .sqlite_store(db_path, None)
        .build()
        .await?;

    info!("restoring session");
    client.restore_session(session.user_session).await?;

    Ok((client, session.sync_token))
}

async fn login(config: Config) -> anyhow::Result<Client> {
    let data_dir = Path::new(&config.data_dir);
    let session_file = data_dir.join("session.json");
    let db_path = data_dir.join("store.db");

    info!("building client");
    let client = Client::builder()
        .homeserver_url(config.homeserver)
        .sqlite_store(db_path, None)
        .build()
        .await?;
    let auth = client.matrix_auth();

    info!("logging in");
    auth.login_username(&config.user_id, &config.password)
        .initial_device_display_name(&config.device_id)
        .await?;
    let user_session = auth
        .session()
        .expect("A logged-in client should have a session");

    debug!("serializing session");
    let serialized_session = serde_json::to_string(&Session {
        user_session,
        sync_token: None,
    })?;

    debug!("storing session");
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
    let MessageType::Text(text_content) = &event.content.msgtype else {
        return;
    };

    let room_name = match room.compute_display_name().await {
        Ok(room_name) => room_name.to_string(),
        Err(error) => {
            info!("Error getting room display name: {error}");
            // Let's fallback to the room ID.
            room.room_id().to_string()
        }
    };

    info!("[{room_name}] {}: {}", event.sender, text_content.body)
}

pub async fn fetch_and_decode_json<D: de::DeserializeOwned>(url: String) -> anyhow::Result<D> {
    let client = RClient::new();

    let data = client.get(url).send().await?;

    Ok(data.json::<D>().await?)
}
