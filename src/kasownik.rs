use crate::{Config, MODULES};

use tracing::{error, info};

use linkme::distributed_slice;
use matrix_sdk::{
    ruma::events::room::message::{
        MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
    },
    Client, Room,
};

use serde_derive::Deserialize;
use serde_json::Value;

use reqwest::Client as RClient;

use leon::{vals, Template};

#[distributed_slice(MODULES)]
static KASOWNIK: fn(&Client, &Config) = callback_registrar;

fn callback_registrar(c: &Client, config: &Config) {
    info!("registering kasownik");

    let url_template_str: String =
        match config.module["kasownik"]["url_template"].clone().try_into() {
            Ok(a) => a,
            Err(e) => {
                info!("Couldn't load url template from configuration: {e}");
                return;
            }
        };

    c.add_event_handler(move |ev, room| due(ev, room, url_template_str));
}

async fn due(ev: OriginalSyncRoomMessageEvent, room: Room, url_template_str: String) {
    let MessageType::Text(text) = ev.content.msgtype else {
        return;
    };

    if !text.body.trim().starts_with(".due") && !text.body.starts_with("~due") {
        return;
    };

    let member: String = if text.body.starts_with(".due-me") || text.body.starts_with("~due-me") {
        ev.sender
            .localpart()
            .trim_start_matches("libera_")
            .to_string()
    } else if text.body.starts_with(".due ") || text.body.starts_with("~due ") {
        text.body
            .trim_start_matches(".due ")
            .trim_start_matches("~due ")
            .to_string()
    } else {
        error!("malformed message: {}", text.body);
        return;
    };

    let url_template = match Template::parse(&url_template_str) {
        Ok(t) => t,
        Err(e) => {
            info!("Couldn't parse url template: {e}");
            return;
        }
    };

    let url = match url_template.render(&&vals(|key| {
        if key == "member" {
            Some(member.clone().into())
        } else {
            None
        }
    })) {
        Ok(u) => u,
        Err(e) => {
            info!("error rendering url template: {e}");
            return;
        }
    };

    let client = RClient::new();
    let response = match client.get(url).send().await {
        Ok(r) => r,
        Err(fe) => {
            info!("error fetching data: {fe}");
            if let Err(se) = room
                .send(RoomMessageEventContent::text_plain("couldn't fetch data"))
                .await
            {
                info!("error sending response: {se}");
            };
            return;
        }
    };

    match response.status().as_u16() {
        404 => _ =room.send(RoomMessageEventContent::text_plain("No such member.")).await,
        410 => _ = room.send(RoomMessageEventContent::text_plain("HTTP 410 Gone.")).await,
        420 => _ = room.send(RoomMessageEventContent::text_plain("HTTP 420 Stoned.")).await,
        200 => {
            let data = match response.json::<Kasownik>().await {
                Ok(d) => d,
                Err(e) => {
                    error!("couldn't decode response: {e}");
                    return;
                }
            };

            if data.status != "ok" {
                error!("no such member? {:#?}", data);
                return;
            };

            let response = match data.content.as_i64() {
                None => {
                    error!("we should not be here: {:#?}", data);
                    return;
                }
                Some(months) => match months {
                    std::i64::MIN..0 => format!("{member} is {} months ahead. Cool!", 0 - months),
                    0 => format!("{member} has paid all their membership fees."),
                    1 => format!("{member} needs to pay one membership fee."),
                    1..=std::i64::MAX => format!("{member} needs to pay {months} membership fees."),
                },
            };

            if let Err(se) = room
                .send(RoomMessageEventContent::text_plain(response))
                .await
            {
                info!("error sending response: {se}");
            };
            return;
        }
        _ => _ = room.send(RoomMessageEventContent::text_plain("wrong status code")).await,
    };
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct Kasownik {
    pub status: String,
    pub content: Value,
    pub modified: String,
}
