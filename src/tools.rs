use anyhow::{anyhow, bail};
use serde::de;
use serde_derive::Deserialize;
use serde_json::Value;

use matrix_sdk::ruma::{OwnedRoomAliasId, OwnedRoomId, OwnedUserId};
use matrix_sdk::{Client, Room};

use reqwest::Client as RClient;

use expiringmap::ExpiringMap;
use std::sync::LazyLock;
use std::time::Duration;
use tokio::sync::Mutex;

static MEMBERSHIPS: LazyLock<Mutex<ExpiringMap<String, MembershipStatus>>> =
    LazyLock::new(Default::default);

/// Possible states of hackerspace membership
#[derive(Clone)]
pub enum MembershipStatus {
    /// User is active and ahead on membership fees (negative values), or behind (positive values)
    Active(i64),
    /// User is known, but is inactive
    Inactive,
    /// Reserved for special cases
    Stoned,
    /// User is not known
    NotAMember,
}

/// Shorthand for making an http request to retrieve a json object, and deserialize it.
pub async fn fetch_and_decode_json<D: de::DeserializeOwned>(url: String) -> anyhow::Result<D> {
    let client = RClient::new();

    let data = client.get(url).send().await?;

    Ok(data.json::<D>().await?)
}

/// Given a string (either an alias, or a room id), try to resolve it to a room object.
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

/// Retrieve the canonical room alias, if known. Otherwise return room id.
pub fn room_name(room: &Room) -> String {
    match room.canonical_alias() {
        Some(a) => a.to_string(),
        None => room.room_id().to_string(),
    }
}

// TODO: implement configurable urls
/// Return membership status for a given user. Best effort.
pub async fn membership_status(user: OwnedUserId) -> anyhow::Result<MembershipStatus> {
    use MembershipStatus::*;
    // TODO: implement membership/mxid mapping
    if user.server_name() != "hackerspace.pl" {
        return Ok(NotAMember);
    };

    let mut memberships = MEMBERSHIPS.lock().await;

    let maybe_member = user.localpart();
    if let Some(membership) = memberships.get(maybe_member) {
        return Ok(membership.to_owned());
    }

    let client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let url = format!("https://kasownik.hackerspace.pl/api/months_due/{maybe_member}.json");
    let response = client.get(url).send().await?;

    let membership = match response.status().as_u16() {
        404 => NotAMember,
        410 => Inactive,
        420 => Stoned,
        200 => {
            let data = response.json::<Kasownik>().await?;

            match data.status.as_str() {
                "ok" => match data.content.as_i64() {
                    None => {
                        bail!("content returned from kasownik doesn't parse as integer: {data:#?}")
                    }
                    Some(months) => Active(months),
                },
                _ => NotAMember,
            }
        }
        _ => bail!("kasownik responded with weird status code",),
    };

    memberships.insert(
        maybe_member.to_string(),
        membership.clone(),
        Duration::from_secs(3 * 60 * 60),
    );

    Ok(membership)
}

/// Kasownik API response structure.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct Kasownik {
    /// Member status. "ok" means they're an active member
    status: String,
    /// Number of late membership fees.
    /// Not an int type as older implementations guarded against data returned here not being parsable as a number.
    content: Value,
    /// Date of last recorded bank transfer in kasownik, globally.
    modified: String,
}

/// Shorter to_string() alias
pub trait ToStringExt: ToString {
    #[allow(missing_docs)]
    fn s(&self) -> String {
        self.to_string()
    }
}

impl<T> ToStringExt for T where T: ToString {}
