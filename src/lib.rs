mod autojoiner;
mod botmanager;
mod config;
mod db;
mod inviter;
mod kasownik;
mod metrics;
mod notbottime;
mod notmun;
mod shenanigans;
mod spaceapi;
mod webterface;
mod wolfram;

pub use crate::botmanager::BotManager;
use crate::notmun::NotMunError;

pub use crate::db::{DBError, DBPools};

use crate::botmanager::{ModuleStarter, WorkerStarter, MODULE_STARTERS, WORKERS};
pub use crate::config::Config;
pub use crate::config::ModuleConfig;

use matrix_sdk::ruma::{OwnedRoomAliasId, OwnedRoomId};
use matrix_sdk::{Client, Room};

use serde::de;

use reqwest::Client as RClient;

pub async fn fetch_and_decode_json<D: de::DeserializeOwned>(url: String) -> anyhow::Result<D> {
    let client = RClient::new();

    let data = client.get(url).send().await?;

    Ok(data.json::<D>().await?)
}

pub(crate) async fn maybe_get_room(c: &Client, maybe_room: &str) -> anyhow::Result<Room> {
    let room_id: OwnedRoomId = match maybe_room.try_into() {
        Ok(r) => r,
        Err(_) => {
            let alias_id = OwnedRoomAliasId::try_from(maybe_room)?;

            c.resolve_room_alias(&alias_id).await?.room_id
        }
    };

    match c.get_room(&room_id) {
        Some(room) => Ok(room),
        // FIXME: replace this with crate-wide errors?
        None => Err(NotMunError::NoRoom(maybe_room.to_string()).into()),
    }
}
