//! Main bot structure
//!
//! Handles the event loop, reloads, logging, and holding Matrix client state.

use core::{error::Error as StdError, fmt};

use std::fs;
use std::ops::Add;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::config::Config;
use crate::tools::room_name;

use matrix_sdk::{
    Client, Error as MatrixError, LoopCtrl, Room,
    authentication::matrix::MatrixSession,
    config::SyncSettings,
    event_handler::EventHandlerHandle,
    ruma::events::room::message::{
        MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
    },
};

use tracing::{debug, error, info, trace};

use anyhow::bail;

use serde_derive::{Deserialize, Serialize};

use futures::future::try_join;
use futures::lock::Mutex;

use tokio::sync::{
    mpsc,
    mpsc::{Receiver, Sender},
};

/// State holding structure for [`BotManager`]
pub struct BotManagerInner {
    config: Config,
    client: Client,
    session_file: PathBuf,
    sync_settings: SyncSettings,
    config_path: String,
    dispatcher_handle: EventHandlerHandle,
    reload_ev_tx: Sender<Room>,
}

/// Possible errors when reloading configuration.
#[derive(Debug)]
pub enum ReloadError {
    /// Configuration file failed to parse. Bot will continue running with the old configuration.
    ConfigParseError(anyhow::Error),
}

impl StdError for ReloadError {}

impl fmt::Display for ReloadError {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::ConfigParseError(e) => write!(fmt, "configuration error: {e}"),
        }
    }
}

impl BotManagerInner {
    /// Handles reload requests
    ///
    /// Attempts to reload configuration file, stop the old event dispatcher, and start [`crate::module::init_modules`]
    /// to handle the rest of the reload process.
    ///
    /// # Errors
    /// Will return `Err` if configuration fails to parse.
    pub fn reload(&mut self) -> Result<(), ReloadError> {
        use ReloadError::ConfigParseError;

        let fenced = self.config.modules_fenced();
        let disabled = self.config.modules_disabled();

        self.config = match Config::new(self.config_path.clone()) {
            Ok(c) => c,
            Err(e) => {
                return Err(ConfigParseError(e));
            }
        };

        for modname in fenced {
            let _ = self.config.fence_module(modname);
        }
        for modname in disabled {
            let _ = self.config.disable_module(modname);
        }

        trace!("config: {:#?}", self.config);

        self.client
            .remove_event_handler(self.dispatcher_handle.clone());
        let dispatcher_handle =
            crate::module::init_modules(&self.client, &self.config, self.reload_ev_tx.clone());

        info!("initialized modules");
        self.dispatcher_handle = dispatcher_handle;

        Ok(())
    }
}

/// Object holding bot state.
///
/// The only value it holds is an Arc<Mutex<>> to the actual state.
pub struct BotManager {
    inner: Arc<Mutex<BotManagerInner>>,
}

impl BotManager {
    /// Bot initialization entrypoint.
    ///
    /// Prepares Matrix [`matrix_sdk::Client`], runs the initialization function for all the modules and workers, initializes default prometheus metrics registry,
    /// and returns to the caller with `BotManager` object started.
    ///
    /// # Errors
    /// Will return `Err` on configuration or matrix client initialization errors.
    pub async fn new(config_path: String, reload_ev_tx: Sender<Room>) -> anyhow::Result<Self> {
        let config = Config::new(config_path.clone())?;
        let data_dir_str = config.data_dir();
        let data_dir = Path::new(&data_dir_str);
        let session_file = data_dir.join("session.json");
        let (client, initial_sync_token) = if session_file.exists() {
            info!("previous session found, attempting restore");
            Self::restore_session(config.clone()).await?
        } else {
            info!("no previous session found, attempting login");
            (Self::login(config.clone()).await?, None)
        };

        let mut sync_settings = SyncSettings::default().full_state(false);
        if let Some(sync_token) = initial_sync_token {
            trace!("initial sync token: {:#?}", &sync_token);
            sync_settings = sync_settings.token(sync_token);
        }

        client.add_event_handler(Self::message_logger);

        debug!("performing initial sync");
        client.sync_once(sync_settings.clone()).await?;

        prometheus::default_registry().register(Box::new(
            tokio_metrics_collector::default_runtime_collector(),
        ))?;

        let dispatcher_handle = crate::module::init_modules(&client, &config, reload_ev_tx.clone());

        info!("finished initializing");

        Ok(Self {
            inner: Arc::new(Mutex::new(BotManagerInner {
                config,
                client,
                session_file,
                sync_settings: sync_settings.clone(),
                config_path,
                dispatcher_handle,
                reload_ev_tx,
            })),
        })
    }

    /// Starts the event loop provided by [`matrix_sdk::Client::sync_with_result_callback`]
    ///
    /// Must be started along with the [`Self::reload`] task for reloads to work.
    ///
    /// # Errors
    /// Will return `Err` if matrix client event loop fails catastrophically.
    pub async fn run(&self) -> anyhow::Result<()> {
        let (sync_settings, session_file, client) = {
            debug!("run: attempting lock");
            let inner = self.inner.lock().await;
            debug!("run: grabbed lock");
            (
                inner.sync_settings.clone(),
                inner.session_file.clone(),
                inner.client.clone(),
            )
        };

        Ok(client
            .sync_with_result_callback(sync_settings, |sync_result| {
                let sfc = session_file.clone();
                async move {
                    trace!("sync response: {:#?}", &sync_result);
                    let response = match sync_result {
                        Ok(r) => r,
                        Err(e) => {
                            error!("sync failed: {e}");
                            return Ok(LoopCtrl::Continue);
                        }
                    };

                    Self::persist_sync_token(&sfc, response.next_batch)
                        .map_err(|err| MatrixError::UnknownError(err.into()))?;
                    Ok(LoopCtrl::Continue)
                }
            })
            .await?)
    }

    async fn restore_session(config: Config) -> anyhow::Result<(Client, Option<String>)> {
        let data_dir_str = config.data_dir();
        let data_dir = Path::new(&data_dir_str);
        let session_file = data_dir.join("session.json");
        let db_path = data_dir.join("store.db");

        let serialized_session = fs::read_to_string(session_file)?;
        trace!("deserializing session");
        let session: BotSession = serde_json::from_str(&serialized_session)?;

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
        let data_dir_str = config.data_dir();
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
        auth.login_username(config.user_id(), &config.password())
            .initial_device_display_name(&config.device_id())
            .await?;
        let user_session = auth
            .session()
            .expect("A logged-in client should have a session");

        trace!("serializing session");
        let serialized_session = serde_json::to_string(&BotSession {
            user_session,
            sync_token: None,
        })?;

        trace!("storing session");
        fs::write(session_file, serialized_session)?;

        Ok(client)
    }

    fn persist_sync_token(session_file: &Path, sync_token: String) -> anyhow::Result<()> {
        let serialized_session = fs::read_to_string(session_file)?;
        let mut session: BotSession = serde_json::from_str(&serialized_session)?;

        session.sync_token = Some(sync_token);
        let serialized_session = serde_json::to_string(&session)?;
        fs::write(session_file, serialized_session)?;

        Ok(())
    }

    /// Logs to stdout text form of the text messages received. Will discard events with invalid
    /// timestamp from the original homeserver, or messages older than 3 seconds.
    #[allow(clippy::unused_async)]
    pub async fn message_logger(event: OriginalSyncRoomMessageEvent, room: Room) {
        let Some(ev_ts) = event.origin_server_ts.to_system_time() else {
            error!("event timestamp couldn't get parsed to system time");
            return;
        };

        if ev_ts.add(Duration::from_secs(3)) < SystemTime::now() {
            trace!("received too old event: {ev_ts:?}");
            return;
        };

        let MessageType::Text(text_content) = &event.content.msgtype else {
            return;
        };

        let room_name = room_name(&room);

        info!("[{room_name}] {}: {}", event.sender, text_content.body);
    }

    /// Main bot entrypoint. Takes config path, and starts everything accordingly.
    ///
    /// # Errors
    /// Will return `Err` if bot encounters an unrecoverable runtime error that didn't
    /// result in process exiting. Unlikely to happen.
    pub async fn serve(config_path: String) -> anyhow::Result<((), ())> {
        let (tx, rx) = mpsc::channel::<matrix_sdk::Room>(1);
        let notbot = &Self::new(config_path, tx).await?;

        let pair = try_join(notbot.reload(rx), notbot.run());

        pair.await
    }

    /// Function that triggers configuration reloading and module reinitialization
    ///
    /// Must be started along with the [`Self::run`] task for reloads to work. When
    /// reload is complete, short information about completion of the task is sent to
    /// the channel from which configuration reload was requested.
    ///
    /// # Errors
    /// Should only return `Err` when event channel is closed
    pub async fn reload(&self, mut rx: Receiver<Room>) -> anyhow::Result<()> {
        loop {
            let Some(room) = rx.recv().await else {
                error!("channel closed, goodbye! :(");
                bail!("channel closed");
            };

            debug!("reload: attempting lock");
            let inner = &mut self.inner.lock().await;
            debug!("reload: grabbed lock");

            let response = match inner.reload() {
                Ok(()) => "configuration reloaded",
                Err(e) => {
                    error!("reload error: {e}");
                    "configuration parsing error, check logs"
                }
            };

            if let Err(e) = room
                .send(RoomMessageEventContent::text_plain(response))
                .await
            {
                error!("sending reload status failed: {e}");
            };
        }
    }
}

/// The full session to persist.
#[derive(Debug, Serialize, Deserialize)]
struct BotSession {
    /// The Matrix user session.
    user_session: MatrixSession,

    /// The latest sync token.
    #[serde(skip_serializing_if = "Option::is_none")]
    sync_token: Option<String>,
}
