//! Various helper and utility functions commonly used in the bot.

use anyhow::{anyhow, bail};
use serde::de;
use serde_derive::Deserialize;
use serde_json::Value;

use matrix_sdk::ruma::{OwnedRoomAliasId, OwnedRoomId, OwnedUserId};
use matrix_sdk::{Client, Room};

use reqwest::{Client as RClient, header};

use expiringmap::ExpiringMap;
use std::sync::LazyLock;
use std::time::Duration;
use tokio::sync::Mutex;

static MEMBERSHIPS: LazyLock<Mutex<ExpiringMap<String, MembershipStatus>>> =
    LazyLock::new(Default::default);

static MXID_HSWAW_MEMBER: LazyLock<Mutex<ExpiringMap<String, String>>> =
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

/// Query the capacifier KVL api; requires Bearer token.
pub async fn capacifier_kvl_query(
    capacifier_token: String,
    req_type: &str,
    req_attr: &str,
    query_attr: &str,
    query_value: String,
) -> anyhow::Result<Option<Vec<String>>> {
    let mut headers = header::HeaderMap::new();
    let mut auth_value =
        header::HeaderValue::from_str(format!("Bearer: {capacifier_token}").as_str())?;
    auth_value.set_sensitive(true);
    headers.insert(header::AUTHORIZATION, auth_value);

    let client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .default_headers(headers)
        .build()?;
    let url = format!(
        "https://capacifier.hackerspace.pl/{req_type}/{req_attr}/{query_attr}/{query_value}"
    );
    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        bail!("wrong capacifier response: {:?}", response.status());
    };
    let data: CapacifierKVL = response.json().await?;

    Ok(data.values)
}

// TODO: implement configurable urls
/// Return membership status for a given user. Best effort.
pub async fn membership_status(
    capacifier_token: String,
    user: OwnedUserId,
) -> anyhow::Result<MembershipStatus> {
    use MembershipStatus::*;

    let mut mxid_map = MXID_HSWAW_MEMBER.lock().await;

    let member: String = match mxid_map.get(user.as_str()) {
        None => {
            match capacifier_kvl_query(
                capacifier_token,
                "kvl",
                "uid",
                "matrixUserID",
                user.to_string(),
            )
            .await
            {
                Ok(Some(v)) => match v.first() {
                    Some(m) => m.to_owned(),
                    None => {
                        return Ok(NotAMember);
                    }
                },
                Ok(None) => {
                    return Ok(NotAMember);
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }
        Some(m) => m.to_owned(),
    };

    // If we got a positive response, we should be fine caching it for some time.
    // Transfering accounts between members shouldn't happen, and if it does, we can
    // handle it by restarting the bot.
    mxid_map.insert(
        user.to_string(),
        member.clone(),
        Duration::from_secs(168 * 60 * 60),
    );

    let mut memberships = MEMBERSHIPS.lock().await;

    let client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let url = format!("https://kasownik.hackerspace.pl/api/months_due/{member}.json");
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

    memberships.insert(member, membership.clone(), Duration::from_secs(3 * 60 * 60));

    Ok(membership)
}

/// Capacifier KVL API response structure
///
/// Equivalent structure in capacifier: <https://code.hackerspace.pl/hswaw/hscloud/src/commit/2e6ad6bafee1e3ff06aef999801e77a14d197e2d/hswaw/capacifier/capacifier.go#L52-L55>
/// Populated in [server.getLdapList()](https://code.hackerspace.pl/hswaw/hscloud/src/commit/2e6ad6bafee1e3ff06aef999801e77a14d197e2d/hswaw/capacifier/capacifier.go#L187-L235)
#[derive(Debug, Clone, Deserialize)]
pub struct CapacifierKVL {
    /// Field that was used to retrieve the values
    pub key: String,
    /// List of values retrieved for a given field
    pub values: Option<Vec<String>>,
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
