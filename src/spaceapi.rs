use crate::{Config, MODULES};

use tracing::{debug, info, trace};

use linkme::distributed_slice;
use matrix_sdk::{
    ruma::{
        events::room::message::{
            MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
        },
        OwnedRoomId,
        RoomAliasId,
    },
    Client, Room, RoomState,
};

use reqwest::{Client as RClient, Url};

use serde::Deserialize;
use std::collections::HashMap;
use tokio::time::{interval, Duration};

#[distributed_slice(MODULES)]
static SPACEAPI: fn(&Client, &Config) = callback_registrar;

fn callback_registrar(c: &Client, config: &Config) {
    info!("registering spaceapi");

    let at_channel_map: HashMap<String, String> = config.module["checkinator"]["Channels"]
        .clone()
        .try_into()
        .expect("checkinator channel map needs to be defined");
    c.add_event_handler(move |ev, room| at_response(ev, room, at_channel_map));

    let presence_channel_map: HashMap<String, String> = config.module["presence"]["Channels"]
        .clone()
        .try_into()
        .expect("presence channel map needs to be defined");

    for (channel, url) in presence_channel_map.into_iter() {
        let room_id = match OwnedRoomId::try_from(channel.clone()) {
            Ok(r) => r,
            Err(e) => {
                info!("channel {} didn't directly map to room id: {}", channel, e);
                continue;
            }
        };

        /* let Ok(room_id) = OwnedRoomId::try_from(channel) else {
            c.resolve_room_alias(&RoomAliasId::parse(channel))
        }; */

        let room = match c.get_room(&room_id) {
            Some(r) => r,
            None => continue,
        };
        presence_observer(room, Url::parse(&url).unwrap());
    }
}

fn presence_observer(room: Room, url: Url) {
    let _ = tokio::task::spawn(async move {
        let client = RClient::new();
        let mut interval = interval(Duration::from_secs(30));
        let mut present: Vec<String> = vec![];

        loop {
            interval.tick().await;

            let json = match client.get(url.clone()).send().await {
                Ok(r) => r,
                Err(_) => {
                    info!("failed to fetch spaceapi data");
                    continue;
                },
            };
            let spaceapi = match json.json::<SpaceAPI>().await {
                Ok(d) => d,
                Err(_) => {
                    info!("failed to decode spaceapi response");
                    continue;
                },
            };

            let current: Vec<String> = names_dehighlighted(spaceapi.sensors.people_now_present);
            let mut arrived: Vec<String> = vec![];
            let mut left: Vec<String> = vec![];
            let mut also_there: Vec<String> = vec![];

            for name in &current {
                if !present.contains(&name) {
                    arrived.push(name.clone());
                };
            }

            for name in &present {
                if current.contains(&name) {
                    also_there.push(name.clone());
                } else {
                    left.push(name.clone());
                };
            }

            present = current;

            let mut response_parts: Vec<String> = vec![];

            if arrived.len() > 0 {
                response_parts.push(["arrived: ", &arrived.join(", ")].concat());
            };

            if left.len() > 0 {
                response_parts.push(["left: ", &left.join(", ")].concat());
            };

            if also_there.len() > 0 {
                response_parts.push(["also there: ", &also_there.join(", ")].concat());
            };

            if arrived.len() == 0 && left.len() == 0 {
                continue;
            };

            room.send(RoomMessageEventContent::notice_plain(
                response_parts.join(", "),
            ))
            .await
            .unwrap();
        }
    });
}

async fn at_response(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    channel_map: HashMap<String, String>,
) {
    trace!("in at_response");
    if room.state() != RoomState::Joined {
        return;
    }

    trace!("checking message type");
    let MessageType::Text(text) = ev.content.msgtype else {
        return;
    };

    trace!("checking if message starts with .at: {:#?}", text.body);
    if text.body.trim().starts_with(".at") {
        debug!("channel_map: {:#?}", channel_map);

        let room_name = match room.compute_display_name().await {
            Ok(room_name) => room_name.to_string(),
            Err(error) => {
                debug!("error getting room display name: {error}");
                // Let's fallback to the room ID.
                room.room_id().to_string()
            }
        };

        debug!("getting spaceapi url for: {:#?}", &room_name);
        let spaceapi_url = match channel_map.get(&room_name) {
            Some(url) => url,
            None => {
                debug!("no spaceapi url found, using default");
                channel_map.get("default").unwrap()
            }
        }
        .clone();

        debug!("spaceapi url: {:#?}", spaceapi_url);

        tokio::spawn(async move {
            let client = RClient::new();
            debug!("fetching url");
            let json = client.get(spaceapi_url).send().await.unwrap();
            debug!("deserializing");
            let spaceapi = json.json::<SpaceAPI>().await.unwrap();

            debug!("spaceapi response: {:#?}", spaceapi);

            let present: Vec<String> = names_dehighlighted(spaceapi.sensors.people_now_present);

            debug!("present: {:#?}", present);

            let response = if present.len() > 0 {
                RoomMessageEventContent::text_plain(present.join(", "))
            } else {
                RoomMessageEventContent::text_plain("Nikdo není doma...")
            };

            room.send(response).await.unwrap();
        });
    };
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

/// only the parts we actually care about
#[derive(Default, Debug, Clone, Deserialize)]
pub struct SpaceAPI {
    pub sensors: Sensors,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct Sensors {
    pub people_now_present: Vec<PeopleNowPresent>,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct PeopleNowPresent {
    pub names: Vec<String>,
}
