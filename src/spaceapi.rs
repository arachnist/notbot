use crate::{Config, MODULES};

use tracing::{debug, info};

use linkme::distributed_slice;
use matrix_sdk::{
    ruma::events::room::message::{
        MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
    },
    Client, Room, RoomState,
};

use reqwest::Client as RClient;

use serde::Deserialize;
use std::collections::HashMap;

#[distributed_slice(MODULES)]
static SPACEAPI: fn(&Client, &Config) = callback_registrar;

fn callback_registrar(c: &Client, config: &Config) {
    info!("registering spaceapi");

    let channel_map: HashMap<String, String> = config.module["checkinator"]["Channels"]
        .clone()
        .try_into()
        .expect("spaceapi channel map needs to be defined");
    c.add_event_handler(move |ev, room| at_response(ev, room, channel_map));
}

async fn at_response(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    channel_map: HashMap<String, String>,
) {
    debug!("in at_response");
    if room.state() != RoomState::Joined {
        return;
    }

    debug!("checking message type");
    let MessageType::Text(text) = ev.content.msgtype else {
        return;
    };

    debug!("checking if message starts with .at: {:#?}", text.body);
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
