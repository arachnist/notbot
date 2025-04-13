// mod autojoiner;
mod inviter;
// mod kasownik;
mod botmanager;
mod notbottime;
pub use crate::botmanager::BotManager;
use crate::botmanager::{ModuleStarter, MODULE_STARTERS};
// mod shenanigans;
// mod spaceapi;
// mod wolfram;
mod config;
pub use crate::config::Config;
pub use crate::config::ModuleConfig;

use serde::de;

use reqwest::Client as RClient;

pub async fn fetch_and_decode_json<D: de::DeserializeOwned>(url: String) -> anyhow::Result<D> {
    let client = RClient::new();

    let data = client.get(url).send().await?;

    Ok(data.json::<D>().await?)
}
