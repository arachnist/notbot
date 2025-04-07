use crate::{fetch_and_decode_json, Config, MODULES};

use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use dashmap::DashMap;
use tracing::{debug, error, info, trace};

use linkme::distributed_slice;
use matrix_sdk::{
    ruma::events::{
        room::message::{MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent},
        Mentions,
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
                error!("Couldn't load url template from configuration: {e}");
                return;
            }
        };

    c.add_event_handler(move |ev, room| due(ev, room, url_template_str));
}

#[distributed_slice(MODULES)]
static DUE_NAG: fn(&Client, &Config) = nag_registrar;

fn nag_registrar(c: &Client, config: &Config) {
    info!("registering kasownik nag");

    let url_template_str: String =
        match config.module["kasownik"]["url_template"].clone().try_into() {
            Ok(a) => a,
            Err(e) => {
                error!("Couldn't load url template from configuration: {e}");
                return;
            }
        };

    // FIXME: accessing config like this can still panic with `index not found`
    let nag_channels: Vec<String> =
        match config.module["kasownik"]["nag_channels"].clone().try_into() {
            Ok(c) => c,
            Err(e) => {
                error!("Couldn't fetch list of nagging channels: {e}");
                return;
            }
        };

    let nag_late_fees: i64 = match config.module["kasownik"]["nag_late_fees"]
        .clone()
        .try_into()
    {
        Ok(c) => c,
        Err(e) => {
            error!("Couldn't fetch late fees setting: {e}");
            return;
        }
    };

    if let Err(e) = DUE_NAG_MAP.set(DashMap::new()) {
        error!("couldn't initialize nag map: {:#?}", e);
        return;
    };

    c.add_event_handler(move |ev, room| {
        due_nag(ev, room, url_template_str, nag_channels, nag_late_fees)
    });
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
            error!("Couldn't parse url template: {e}");
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
            error!("error rendering url template: {e}");
            return;
        }
    };

    let client = RClient::new();
    let response = match client.get(url).send().await {
        Ok(r) => r,
        Err(fe) => {
            error!("error fetching data: {fe}");
            if let Err(se) = room
                .send(RoomMessageEventContent::text_plain("couldn't fetch data"))
                .await
            {
                error!("error sending response: {se}");
            };
            return;
        }
    };

    match response.status().as_u16() {
        404 => {
            _ = room
                .send(RoomMessageEventContent::text_plain("No such member."))
                .await
        }
        410 => {
            _ = room
                .send(RoomMessageEventContent::text_plain("HTTP 410 Gone."))
                .await
        }
        420 => {
            _ = room
                .send(RoomMessageEventContent::text_plain("HTTP 420 Stoned."))
                .await
        }
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
                error!("error sending response: {se}");
            };
            return;
        }
        _ => {
            _ = room
                .send(RoomMessageEventContent::text_plain("wrong status code"))
                .await
        }
    };
}

static DUE_NAG_MAP: OnceLock<DashMap<String, SystemTime>> = OnceLock::new();

async fn due_nag(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    url_template_str: String,
    nag_channels: Vec<String>,
    nag_late_fees: i64,
) {
    if let Some(alias) = room.canonical_alias() {
        if !nag_channels.iter().any(|x| x == alias.as_str()) {
            return;
        };
    } else {
        return;
    };

    let nag_map = match DUE_NAG_MAP.get() {
        Some(m) => m,
        None => {
            error!("nag map not initialized, this shouldn't have happened");
            return;
        }
    };

    let sender_str: &str = ev.sender.as_str();
    let member: String = ev.sender.localpart().to_owned();

    let next_nag_time = if nag_map.contains_key(sender_str) {
        trace!("nag map knows {sender_str}");
        *nag_map.get(sender_str).unwrap().value()
    } else {
        trace!("nag map doesn't know {sender_str}");
        let nag_time = SystemTime::now() - Duration::new(60 * 60 * 24, 0);
        nag_map.insert(sender_str.to_owned(), nag_time);

        nag_time
    };

    if SystemTime::now() > next_nag_time {
        debug!("member not checked recently: {sender_str}");
        nag_map.alter(sender_str, |_, _| {
            SystemTime::now() + Duration::new(60 * 60 * 24, 0)
        });
    } else {
        debug!("member checked recently, ignoring: {sender_str}");
        return;
    };

    let url_template = match Template::parse(&url_template_str) {
        Ok(t) => t,
        Err(e) => {
            error!("Couldn't parse url template: {e}");
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
            error!("error rendering url template: {e}");
            return;
        }
    };

    let data: Kasownik = match fetch_and_decode_json(url).await {
        Ok(d) => d,
        Err(e) => {
            error!("couldn't fetch kasownik data: {e}");
            return;
        }
    };
    trace!("returned data: {:#?}", data);

    if let Some(months) = data.content.as_i64() {
        if months < nag_late_fees {
            debug!("too early to nag: {months}");
            return;
        };

        let period = match months {
            std::i64::MIN..=0 => {
                return;
            }
            1 => "month",
            _ => "months",
        };

        let member_display_name: String = match room.get_member(&ev.sender).await {
            Ok(Some(rm)) => match rm.display_name() {
                Some(d) => d.to_owned(),
                None => sender_str.to_owned(),
            },
            _ => sender_str.to_owned(),
        };

        let msg_text = format!("pay your membership fees! you are {months} {period} behind!");
        let plain_message = format!(
            r#"{display_name}: {text}"#,
            display_name = member_display_name,
            text = msg_text
        );

        let html_message = format!(
            r#"<a href="{uri}">{display_name}</a>: {text}"#,
            uri = ev.sender.matrix_to_uri(),
            display_name = member_display_name,
            text = msg_text
        );

        let msg = RoomMessageEventContent::text_html(plain_message, html_message)
            .add_mentions(Mentions::with_user_ids(vec![ev.sender]));

        if let Err(e) = room.send(msg).await {
            error!("error sending message: {e}")
        }
    }
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct Kasownik {
    pub status: String,
    pub content: Value,
    pub modified: String,
}
