use crate::prelude::*;

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

    match c.get_room(&room_id) {
        Some(room) => Ok(room),
        // FIXME: replace this with crate-wide errors?
        None => Err(NotMunError::NoRoom(maybe_room.to_string()).into()),
    }
}
