// mod autojoiner;
// mod inviter;
// mod kasownik;
// mod notbottime;
// mod pluginmanager;
// mod shenanigans;
// mod spaceapi;
// mod wolfram;

use anyhow::{anyhow, Context};
use tracing::{debug, error, info, trace};

use serde::{de, Deserialize, Serialize};

use matrix_sdk::{
    authentication::matrix::MatrixSession,
    config::SyncSettings,
    ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent},
    Client, Error, LoopCtrl, Room,
};

use reqwest::Client as RClient;

use core::{error::Error as StdError, fmt};
use std::{
    convert::TryFrom,
    fs,
    future::Future,
    io,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex},
};
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

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    NoPath(&'static str),
}

impl StdError for ConfigError {}

impl From<io::Error> for ConfigError {
    fn from(e: io::Error) -> Self {
        ConfigError::Io(e)
    }
}

impl From<toml::de::Error> for ConfigError {
    fn from(e: toml::de::Error) -> Self {
        ConfigError::Parse(e)
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ConfigError::Io(e) => write!(fmt, "IO error: {}", e),
            ConfigError::Parse(e) => write!(fmt, "parsing error: {}", e),
            ConfigError::NoPath(e) => write!(fmt, "No config path provided: {}", e),
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct ConfigInner {
    pub(crate) homeserver: String,
    pub(crate) user_id: String,
    pub(crate) password: String,
    pub(crate) data_dir: String,
    pub(crate) device_id: String,
    #[allow(dead_code)]
    pub(crate) module: Table,
}

impl TryFrom<String> for ConfigInner {
    type Error = ConfigError;
    fn try_from(path: String) -> Result<Self, Self::Error> {
        let config_content = fs::read_to_string(&path)?;
        Ok(toml::from_str::<ConfigInner>(&config_content)?)
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    inner: Arc<Mutex<ConfigInner>>,
}

impl TryFrom<String> for Config {
    type Error = ConfigError;
    fn try_from(path: String) -> Result<Self, Self::Error> {
        Ok(Config {
            inner: Arc::new(Mutex::new(path.try_into()?)),
        })
    }
}

impl TryFrom<Option<String>> for Config {
    type Error = ConfigError;
    fn try_from(maybe_path: Option<String>) -> Result<Self, Self::Error> {
        let path = match maybe_path {
            Some(p) => p,
            None => return Err(ConfigError::NoPath("no path provided")),
        };

        Ok(path.try_into()?)
    }
}

impl Config {
    #[allow(dead_code)]
    pub fn reload(mut self, path: String) -> anyhow::Result<Config> {
        let new_cfg = Arc::new(Mutex::new(TryInto::<ConfigInner>::try_into(path)?));
        self.inner = new_cfg;
        Ok(self)
    }

    pub fn homeserver(&self) -> String {
        let inner = &self.inner.lock().unwrap();
        inner.homeserver.clone()
    }

    pub fn user_id(&self) -> String {
        let inner = &self.inner.lock().unwrap();
        inner.user_id.clone()
    }

    pub fn password(&self) -> String {
        let inner = &self.inner.lock().unwrap();
        inner.password.clone()
    }

    pub fn data_dir(&self) -> String {
        let inner = &self.inner.lock().unwrap();
        inner.data_dir.clone()
    }

    pub fn device_id(&self) -> String {
        let inner = &self.inner.lock().unwrap();
        inner.device_id.clone()
    }
}

pub async fn run(config: Config) -> anyhow::Result<()> {
    let data_dir_str = &config.data_dir();
    let data_dir = Path::new(&data_dir_str);
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
        trace!("initial sync token: {:#?}", &sync_token);
        sync_settings = sync_settings.token(sync_token);
    }

    trace!("adding config as extra context for callbacks");
    client.add_event_handler_context(config.clone());

    debug!("performing initial sync");
    client.sync_once(SyncSettings::default()).await.unwrap();

    client.add_event_handler(on_room_message);

    for initializer in MODULES {
        initializer(&client, &config);
    }

    debug!("finished initializing");
    client
        .sync_with_result_callback(sync_settings, |sync_result| {
            let session_path = session_file.clone();
            async move {
                trace!("sync response: {:#?}", &sync_result);
                let response = match sync_result {
                    Ok(r) => r,
                    Err(e) => {
                        error!("sync failed: {e}");
                        return Ok(LoopCtrl::Continue);
                    }
                };

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
    let data_dir_str = &config.data_dir();
    let data_dir = Path::new(&data_dir_str);
    let session_file = data_dir.join("session.json");
    let db_path = data_dir.join("store.db");

    let serialized_session = fs::read_to_string(session_file)?;
    trace!("deserializing session");
    let session: Session = serde_json::from_str(&serialized_session)?;

    trace!("building client");
    let client = Client::builder()
        .homeserver_url(config.homeserver())
        .sqlite_store(db_path, None)
        .build()
        .await?;

    trace!("restoring session");
    client.restore_session(session.user_session).await?;

    Ok((client, session.sync_token))
}

async fn login(config: Config) -> anyhow::Result<Client> {
    let data_dir_str = &config.data_dir();
    let data_dir = Path::new(&data_dir_str);
    let session_file = data_dir.join("session.json");
    let db_path = data_dir.join("store.db");

    trace!("building client");
    let client = Client::builder()
        .homeserver_url(config.homeserver())
        .sqlite_store(db_path, None)
        .build()
        .await?;
    let auth = client.matrix_auth();

    trace!("logging in");
    auth.login_username(&config.user_id(), &config.password())
        .initial_device_display_name(&config.device_id())
        .await?;
    let user_session = auth
        .session()
        .expect("A logged-in client should have a session");

    trace!("serializing session");
    let serialized_session = serde_json::to_string(&Session {
        user_session,
        sync_token: None,
    })?;

    trace!("storing session");
    fs::write(session_file, serialized_session)?;

    Ok(client)
}

///// handler copypasted from examples
/// log room messages.
async fn on_room_message(event: OriginalSyncRoomMessageEvent, room: Room) {
    let MessageType::Text(text_content) = &event.content.msgtype else {
        return;
    };

    let room_name = match room.canonical_alias() {
        Some(a) => a.to_string(),
        None => room.room_id().to_string(),
    };

    info!("[{room_name}] {}: {}", event.sender, text_content.body)
}

pub async fn fetch_and_decode_json<D: de::DeserializeOwned>(url: String) -> anyhow::Result<D> {
    let client = RClient::new();

    let data = client.get(url).send().await?;

    Ok(data.json::<D>().await?)
}
