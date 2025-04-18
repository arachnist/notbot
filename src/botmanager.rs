use crate::Config;

use futures::lock::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::{
    fs,
    path::{Path, PathBuf},
};
use tokio::task::AbortHandle;

use tracing::{debug, error, info, trace, warn};

use matrix_sdk::event_handler::EventHandlerHandle;
use matrix_sdk::Client;

use matrix_sdk::{
    authentication::matrix::MatrixSession,
    config::SyncSettings,
    ruma::events::room::message::{
        MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
    },
    Error as MatrixError, LoopCtrl, Room,
};

use tokio::sync::broadcast::{channel, Receiver, Sender};

use serde::{Deserialize, Serialize};

use linkme::distributed_slice;

use lazy_static::lazy_static;
use prometheus::Counter;
use prometheus::{opts, register_counter};

lazy_static! {
    static ref CONFIG_RELOADS: Counter = register_counter!(opts!(
        "config_reloads_total",
        "Number of DB status requests",
    ))
    .unwrap();
}

pub type WorkerStarter = (
    &'static str,
    fn(&Client, &Config) -> anyhow::Result<AbortHandle>,
);

#[distributed_slice]
pub static WORKERS: [WorkerStarter] = [..];

pub struct Worker {
    handle: Option<AbortHandle>,
    starter: fn(&Client, &Config) -> anyhow::Result<AbortHandle>,
}

pub type ModuleStarter = (
    &'static str,
    fn(&Client, &Config) -> anyhow::Result<EventHandlerHandle>,
);

#[distributed_slice]
pub static MODULE_STARTERS: [ModuleStarter] = [..];

pub struct Module {
    handle: Option<EventHandlerHandle>,
    starter: fn(&Client, &Config) -> anyhow::Result<EventHandlerHandle>,
}

struct BotManagerInner {
    modules: HashMap<String, Module>,
    workers: HashMap<String, Worker>,
    config: Config,
    client: Client,
    session_file: PathBuf,
    sync_settings: SyncSettings,
    config_path: String,
}

impl BotManagerInner {
    pub async fn reload(&mut self) -> anyhow::Result<String> {
        info!("reloading");

        self.config = match Config::new(self.config_path.clone()) {
            Ok(c) => c,
            Err(e) => {
                error!("couldn't parse configuration: {e}");
                return Err(e);
            }
        };

        let mut failed_modules: Vec<String> = vec![];
        let mut failed_workers: Vec<String> = vec![];

        trace!("config: {:#?}", self.config);

        for (name, module) in &mut self.modules {
            match &module.handle {
                Some(handle) => {
                    info!("unregistering\t{name}");
                    self.client.remove_event_handler(handle.to_owned());
                }
                None => info!("module was previously not registerd: {name}"),
            };

            info!("registering:\t{name}");

            let handle = match (module.starter)(&self.client, &self.config) {
                Ok(h) => Some(h),
                Err(e) => {
                    error!("initializing module failed: {name} {e}");
                    failed_modules.push(name.to_owned());
                    None
                }
            };

            module.handle = handle;
        }

        for (name, worker) in &mut self.workers {
            match &worker.handle {
                Some(handle) => {
                    info!("stopping: {name}");
                    handle.abort();
                }
                None => info!("worker was previously not started: {name}"),
            };

            info!("starting: {name}");

            let handle = match (worker.starter)(&self.client, &self.config) {
                Ok(h) => Some(h),
                Err(e) => {
                    error!("initializing worker failed: {name} {e}");
                    failed_workers.push(name.to_owned());
                    None
                }
            };

            worker.handle = handle;
        }

        let mut status: String = "configuration reloaded".to_string();

        if failed_workers.is_empty() && failed_modules.is_empty() {
            return Ok(status);
        } else {
            status.push_str(", but:");
        };

        if !failed_modules.is_empty() {
            status.push_str(
                format!(
                    " {len} module{maybe_plural} failed: {modules}",
                    len = failed_modules.len(),
                    modules = failed_modules.join(", "),
                    maybe_plural = if failed_modules.len() == 1 { "" } else { "s" }
                )
                .as_str(),
            );
        };

        if !failed_workers.is_empty() {
            status.push_str(
                format!(
                    "{maybe_conjunction} {len} worker{maybe_plural} failed: {workers}",
                    maybe_conjunction = if failed_modules.is_empty() {
                        ""
                    } else {
                        ", and"
                    },
                    len = failed_workers.len(),
                    workers = failed_workers.join(", "),
                    maybe_plural = if failed_workers.len() == 1 { "" } else { "s" }
                )
                .as_str(),
            );
        };

        Ok(status)
    }
}

pub struct BotManager {
    inner: Arc<Mutex<BotManagerInner>>,
}

impl BotManager {
    pub async fn new(config_path: String) -> anyhow::Result<(Self, Sender<Room>)> {
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

        let mut modules: HashMap<String, Module> = Default::default();

        for (name, starter) in MODULE_STARTERS {
            let handle: Option<EventHandlerHandle> = match starter(&client, &config) {
                Ok(h) => Some(h),
                Err(e) => {
                    error!("initializing module {name} failed: {e}");
                    None
                }
            };

            info!("registering: {name}");

            modules.insert(
                name.to_string(),
                Module {
                    handle,
                    starter: *starter,
                },
            );
        }

        let mut workers: HashMap<String, Worker> = Default::default();

        for (name, starter) in WORKERS {
            let handle: Option<AbortHandle> = match starter(&client, &config) {
                Ok(h) => Some(h),
                Err(e) => {
                    error!("initializing worker {name} failed: {e}");
                    None
                }
            };

            info!("registering worker: {name}");

            workers.insert(
                name.to_string(),
                Worker {
                    handle,
                    starter: *starter,
                },
            );
        }

        let (tx, _) = channel::<Room>(1);

        let tx2 = tx.clone();

        // this config will not be reloadable :(
        let reloader_config: ReloaderConfig =
            config.module_config_value("notbot::reloader")?.try_into()?;
        let _ = client.add_event_handler(move |ev, room| {
            Self::reload_trigger(ev, room, tx.clone(), reloader_config)
        });

        info!("finished initializing");

        Ok((
            BotManager {
                inner: Arc::new(Mutex::new(BotManagerInner {
                    modules,
                    workers,
                    config,
                    client,
                    session_file,
                    sync_settings: sync_settings.clone(),
                    config_path,
                })),
            },
            tx2,
        ))
    }

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
                        .await
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

    async fn persist_sync_token(session_file: &Path, sync_token: String) -> anyhow::Result<()> {
        let serialized_session = fs::read_to_string(session_file)?;
        let mut session: BotSession = serde_json::from_str(&serialized_session)?;

        session.sync_token = Some(sync_token);
        let serialized_session = serde_json::to_string(&session)?;
        fs::write(session_file, serialized_session)?;

        Ok(())
    }

    async fn message_logger(event: OriginalSyncRoomMessageEvent, room: Room) {
        let MessageType::Text(text_content) = &event.content.msgtype else {
            return;
        };

        let room_name = match room.canonical_alias() {
            Some(a) => a.to_string(),
            None => room.room_id().to_string(),
        };

        info!("[{room_name}] {}: {}", event.sender, text_content.body)
    }

    async fn reload_trigger(
        event: OriginalSyncRoomMessageEvent,
        room: Room,
        tx: Sender<Room>,
        module_config: ReloaderConfig,
    ) -> anyhow::Result<()> {
        let MessageType::Text(text) = event.content.msgtype else {
            return Ok(());
        };

        if !text.body.trim().starts_with(".reload") {
            return Ok(());
        };

        if module_config.admins.contains(&event.sender.to_string()) {
            if tx.is_empty() {
                info!("sending reload trigger");
                tx.send(room)?;
            } else {
                warn!("already processing reload request");
                return Ok(());
            };
        };

        Ok(())
    }

    pub async fn reload(&self, mut rx: Receiver<Room>) -> anyhow::Result<()> {
        loop {
            let room: Room = rx.recv().await?;
            CONFIG_RELOADS.inc();
            debug!("reload: attempting lock");
            let inner = &mut self.inner.lock().await;
            debug!("reload: grabbed lock");
            let status: String = match inner.reload().await {
                Ok(s) => s,
                Err(e) => format!(
                    "configuration not reloaded: {message}",
                    message = e.to_string().lines().nth(0).unwrap_or("couldn't decode")
                ),
            };
            if let Err(e) = room.send(RoomMessageEventContent::text_plain(status)).await {
                error!("sending reload status failed: {e}");
            };
        }
    }
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct ReloaderConfig {
    admins: Vec<String>,
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
