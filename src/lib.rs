mod spaceapi;

use anyhow::{Context, anyhow};
use serde::{Deserialize, Serialize};

use matrix_sdk::{
    matrix_auth::MatrixSession,
    config::SyncSettings,
    Client,
};

use std::{
    fs,
    path::Path,
};

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
    let (client, sync_token) = if session_file.exists() {
        tracing::info!("previous session found, attempting restore");
        restore_session(config).await?
    } else {
        tracing::info!("no previous session found, attempting login");
        (login(config).await?, None)
    };

    tracing::info!("performing initial sync to ignore old messages...");
    client.sync_once(SyncSettings::default()).await.unwrap();
    tracing::info!("performing initial sync to ignore old messages...");
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
