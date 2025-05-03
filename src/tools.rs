use serde::de;
use anyhow::anyhow;

use matrix_sdk::{Client, Room};
use matrix_sdk::ruma::{OwnedRoomId, OwnedRoomAliasId};

use reqwest::Client as RClient;

pub async fn fetch_and_decode_json<D: de::DeserializeOwned>(url: String) -> anyhow::Result<D> {
    let client = RClient::new();

    let data = client.get(url).send().await?;

    Ok(data.json::<D>().await?)
}

pub async fn maybe_get_room(c: &Client, maybe_room: &str) -> anyhow::Result<Room> {
    let room_id: OwnedRoomId = match maybe_room.try_into() {
        Ok(r) => r,
        Err(_) => {
            let alias_id = OwnedRoomAliasId::try_from(maybe_room)?;

            c.resolve_room_alias(&alias_id).await?.room_id
        }
    };
    
    c.get_room(&room_id).ok_or(anyhow!("no room"))
}

pub fn get_room_name(room: &Room) -> String {
    match room.canonical_alias() {
        Some(a) => a.to_string(),
        None => room.room_id().to_string(),
    }
}

pub mod sync {
    use serde::de;
    use reqwest::blocking::Client as RClient;

    pub fn fetch_and_decode_json<D: de::DeserializeOwned>(url: String)->anyhow::Result<D> {
        let client = RClient::new();
        let data = client.get(url).send()?;
        Ok(data.json::<D>()?)
    }
}
