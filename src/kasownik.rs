use crate::{
    fetch_and_decode_json, notbottime::NotBotTime, Config, ModuleStarter, MODULE_STARTERS,
};

use std::time::{Duration, SystemTime};

use tracing::{debug, error, trace};

use linkme::distributed_slice;
use matrix_sdk::{
    event_handler::EventHandlerHandle,
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

// module_path!().to_string() + "_nag"

#[distributed_slice(MODULE_STARTERS)]
static MODULE_STARTER_NAG: ModuleStarter = ("notbot::kasownik_nag", module_starter_nag);

fn module_starter_nag(client: &Client, config: &Config) -> anyhow::Result<EventHandlerHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
    Ok(client
        .add_event_handler(move |ev, room, c| module_entrypoint_nag(ev, room, c, module_config)))
}

#[distributed_slice(MODULE_STARTERS)]
static MODULE_STARTER: ModuleStarter = (module_path!(), module_starter);

fn module_starter(client: &Client, config: &Config) -> anyhow::Result<EventHandlerHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
    Ok(client.add_event_handler(move |ev, room| module_entrypoint(ev, room, module_config)))
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct ModuleConfig {
    pub url_template: String,
    pub nag_channels: Vec<String>,
    pub nag_late_fees: i64,
}

async fn module_entrypoint(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    module_config: ModuleConfig,
) {
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

    let url_template = match Template::parse(&module_config.url_template) {
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

async fn module_entrypoint_nag(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    c: Client,
    module_config: ModuleConfig,
) {
    if let Some(alias) = room.canonical_alias() {
        if !module_config
            .nag_channels
            .iter()
            .any(|x| x == alias.as_str())
        {
            return;
        };
    } else {
        return;
    };

    let sender_str: &str = ev.sender.as_str();
    let member: String = ev.sender.localpart().to_owned();

    trace!("getting client store");
    let store = c.state_store();
    // let maybe_next_nag_time = store.get_custom_value(ev.sender.as_bytes()).await;
    let next_nag_time = match store.get_custom_value(ev.sender.as_bytes()).await {
        Ok(maybe_result) => {
            let maybe_nag_time = match maybe_result {
                None => {
                    let nag_time = NotBotTime(SystemTime::now() - Duration::new(60 * 60 * 24, 0));
                    let nag_time_bytes: Vec<u8> = nag_time.into();

                    if let Err(e) = store
                        .set_custom_value_no_read(ev.sender.as_bytes(), nag_time_bytes)
                        .await
                    {
                        error!("error setting nag time value for the first time: {e}");
                        return;
                    };

                    nag_time
                }
                Some(nag_time_bytes) => nag_time_bytes.into(),
            };

            maybe_nag_time
        }
        Err(e) => {
            error!("error fetching nag time: {e}");
            return;
        }
    };

    trace!("next_nag_time: {:#?}", next_nag_time);

    if NotBotTime::now() > next_nag_time {
        debug!("member not checked recently: {sender_str}");
        let next_nag_time = NotBotTime(SystemTime::now() + Duration::new(60 * 60 * 24, 0));

        if let Err(e) = store
            .set_custom_value_no_read(ev.sender.as_bytes(), next_nag_time.into())
            .await
        {
            error!("error setting nag time value for the first time: {e}");
            return;
        };
    } else {
        debug!("member checked recently, ignoring: {sender_str}");
        return;
    };

    let url_template = match Template::parse(&module_config.url_template) {
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
        if months < module_config.nag_late_fees {
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
