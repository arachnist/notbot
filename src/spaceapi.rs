use crate::{
    fetch_and_decode_json, Config, ModuleStarter, WorkerStarter, MODULE_STARTERS, WORKERS,
};

use tokio::task::AbortHandle;
use tracing::{debug, error, trace};

use linkme::distributed_slice;
use matrix_sdk::{
    event_handler::EventHandlerHandle,
    ruma::{
        events::room::message::{
            MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
        },
        OwnedRoomAliasId, OwnedRoomId,
    },
    Client, Room, RoomState,
};

use serde::Deserialize;
use std::collections::HashMap;
use tokio::time::{interval, Duration};

#[distributed_slice(MODULE_STARTERS)]
static MODULE_STARTER: ModuleStarter = (module_path!(), module_starter);

fn module_starter(client: &Client, config: &Config) -> anyhow::Result<EventHandlerHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
    Ok(client.add_event_handler(move |ev, room| module_entrypoint(ev, room, module_config)))
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct ModuleConfig {
    room_map: HashMap<String, String>,
    presence_map: HashMap<String, Vec<String>>,
    presence_interval: u64,
}

#[distributed_slice(WORKERS)]
static WORKER_STARTER: WorkerStarter = (module_path!(), worker_starter);

fn worker_starter(client: &Client, config: &Config) -> anyhow::Result<AbortHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
    let worker = tokio::task::spawn(presence_observer(client.clone(), module_config));
    Ok(worker.abort_handle())
}

async fn presence_observer(client: Client, module_config: ModuleConfig) {
    // let  = tokio::task::spawn(async move {
    let mut interval = interval(Duration::from_secs(module_config.presence_interval));
    let mut present: HashMap<String, Vec<String>> = Default::default();
    let mut first_loop: HashMap<String, bool> = Default::default();

    for url in module_config.presence_map.keys() {
        present.insert(url.clone(), vec![]);
        first_loop.insert(url.clone(), true);
    }

    loop {
        interval.tick().await;

        for (url, rooms) in &module_config.presence_map {
            debug!("fetching spaceapi url: {}", url);
            let data = match fetch_and_decode_json::<SpaceAPI>(url.to_owned()).await {
                Ok(d) => d,
                Err(fe) => {
                    error!("error fetching data: {fe}");
                    continue;
                }
            };

            let current: Vec<String> = names_dehighlighted(data.sensors.people_now_present);
            let mut arrived: Vec<String> = vec![];
            let mut left: Vec<String> = vec![];
            let mut also_there: Vec<String> = vec![];

            for name in &current {
                if !present[url].contains(name) {
                    arrived.push(name.clone());
                };
            }

            for name in &present[url] {
                if current.contains(name) {
                    also_there.push(name.clone());
                } else {
                    left.push(name.clone());
                };
            }

            present.insert(url.clone(), current);

            if first_loop[url] {
                first_loop.insert(url.clone(), false);
                continue;
            }

            let mut response_parts: Vec<String> = vec![];

            if !arrived.is_empty() {
                response_parts.push(["arrived: ", &arrived.join(", ")].concat());
            };

            if !left.is_empty() {
                response_parts.push(["left: ", &left.join(", ")].concat());
            };

            if !also_there.is_empty() {
                response_parts.push(["also there: ", &also_there.join(", ")].concat());
            };

            if arrived.is_empty() && left.is_empty() {
                continue;
            };

            let response = RoomMessageEventContent::notice_plain(response_parts.join(", "));
            for maybe_room in rooms {
                let room_id: OwnedRoomId = match maybe_room.clone().try_into() {
                    Ok(r) => r,
                    Err(_) => {
                        let alias_id = match OwnedRoomAliasId::try_from(maybe_room.clone()) {
                            Ok(a) => a,
                            Err(_) => {
                                error!("couldn't parse room name: {}", maybe_room);
                                continue;
                            }
                        };

                        match client.resolve_room_alias(&alias_id).await {
                            Err(e) => {
                                error!("couldn't resolve room alias: {e}");
                                continue;
                            }
                            Ok(r) => r.room_id,
                        }
                    }
                };

                let room = match client.get_room(&room_id) {
                    Some(r) => r,
                    None => continue,
                };

                if let Err(e) = room.send(response.clone()).await {
                    error!("error while sending presence update: {e}");
                };
            }
        }
    }
}

async fn module_entrypoint(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    module_config: ModuleConfig,
) -> anyhow::Result<()> {
    trace!("in at_response");
    if room.state() != RoomState::Joined {
        return Ok(());
    }

    trace!("checking message type");
    let MessageType::Text(text) = ev.content.msgtype else {
        return Ok(());
    };

    trace!("checking if message starts with .at: {:#?}", text.body);
    if text.body.trim().starts_with(".at") {
        let room_name = match room.canonical_alias() {
            Some(name) => name.to_string(),
            None => room.room_id().to_string(),
        };

        let url = match module_config.room_map.get(&room_name) {
            Some(url) => url,
            None => {
                debug!("no spaceapi url found, using default");
                match module_config.room_map.get("default") {
                    None => return Ok(()),
                    Some(u) => u,
                }
            }
        };

        let data = match fetch_and_decode_json::<SpaceAPI>(url.to_owned()).await {
            Ok(d) => d,
            Err(fe) => {
                error!("error fetching data: {fe}");
                if let Err(se) = room
                    .send(RoomMessageEventContent::text_plain("couldn't fetch data"))
                    .await
                {
                    error!("error sending response: {se}");
                };
                return Ok(());
            }
        };

        let present: Vec<String> = names_dehighlighted(data.sensors.people_now_present);

        debug!("present: {:#?}", present);

        let response = if !present.is_empty() {
            RoomMessageEventContent::text_plain(present.join(", "))
        } else {
            RoomMessageEventContent::text_plain("Nikdo není doma...")
        };

        room.send(response).await?;
    };

    Ok(())
}

fn names_dehighlighted(present: Vec<PeopleNowPresent>) -> Vec<String> {
    let mut dehighlighted: Vec<String> = vec![];

    for sensor in present {
        for name in sensor.names {
            let mut chars = name.chars();
            dehighlighted.push(match chars.next() {
                None => String::new(),
                Some(first) => first.to_string() + "\u{200B}" + chars.as_str(),
            });
        }
    }

    dehighlighted
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct SpaceAPI {
    pub api_compatibility: Vec<String>,
    pub space: String,
    pub logo: String,
    pub url: String,
    pub location: Location,
    pub state: State,
    pub contact: Contact,
    pub projects: Vec<String>,
    pub feeds: Feeds,
    pub sensors: Sensors,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct Location {
    pub lat: f64,
    pub lon: f64,
    pub address: String,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct State {
    pub open: bool,
    pub message: String,
    pub icon: Icon,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct Icon {
    pub open: String,
    pub closed: String,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct Contact {
    pub facebook: String,
    pub irc: String,
    pub mastodon: String,
    pub matrix: String,
    pub ml: String,
    pub twitter: String,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct Feeds {
    pub blog: Blog,
    pub calendar: Calendar,
    pub wiki: Wiki,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct Blog {
    #[serde(rename = "type")]
    pub type_field: String,
    pub url: String,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct Calendar {
    #[serde(rename = "type")]
    pub type_field: String,
    pub url: String,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct Wiki {
    #[serde(rename = "type")]
    pub type_field: String,
    pub url: String,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct Sensors {
    pub people_now_present: Vec<PeopleNowPresent>,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct PeopleNowPresent {
    pub value: u32,
    pub names: Vec<String>,
}
