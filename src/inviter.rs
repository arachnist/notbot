use std::time::{Duration, SystemTime};

use crate::{
    notbottime::{NotBotTime, NOTBOT_EPOCH},
    Config, MODULES,
};

use tracing::{debug, error, info, trace};

use linkme::distributed_slice;
use matrix_sdk::{
    ruma::{
        events::{
            room::message::{MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent},
            Mentions,
        },
        RoomAliasId,
    },
    Client, Room,
};

use serde_derive::Deserialize;

#[distributed_slice(MODULES)]
static INVITER: fn(&Client, &Config) = callback_registrar;

fn callback_registrar(c: &Client, config: &Config) {
    info!("registering inviter");

    let inviter_config: InviterConfig = match config.module["inviter"].clone().try_into() {
        Ok(a) => a,
        Err(e) => {
            error!("Couldn't load url template from configuration: {e}");
            return;
        }
    };

    c.add_event_handler(move |ev, room, client| {
        inviter_listener(
            ev,
            room,
            client,
            inviter_config.requests,
            inviter_config.confirmations,
        )
    });
}

async fn inviter_listener(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    client: Client,
    requests_room: String,
    confirmations_room: String,
) {
    if let Some(alias) = room.canonical_alias() {
        if requests_room != alias.as_str() {
            return;
        };
    } else {
        return;
    };

    let MessageType::Text(text) = ev.content.msgtype else {
        return;
    };

    if text.body.trim().starts_with(".invite") {
        return;
    };

    if ev.sender == client.user_id().unwrap() {
        return;
    };

    let prefixed_sender = "inviter:".to_owned() + ev.sender.as_str();
    let store = client.store();

    let next_allowed_attempt = match store.get_custom_value(prefixed_sender.as_bytes()).await {
        Err(e) => {
            error!("error fetching nag time: {e}");
            return;
        }
        Ok(maybe_result) => {
            let next_attempt: NotBotTime = match maybe_result {
                Some(req_time_bytes) => req_time_bytes.into(),
                None => NOTBOT_EPOCH,
            };

            next_attempt
        }
    };

    // from_days() is experimental :(
    if NotBotTime::now() > next_allowed_attempt {
        let next_allowed_attempt =
            NotBotTime(SystemTime::now() + Duration::from_secs(60 * 60 * 24 * 7));
        if let Err(e) = store
            .set_custom_value_no_read(prefixed_sender.as_bytes(), next_allowed_attempt.into())
            .await
        {
            error!(
                "error setting last request time for user: {} {e}",
                ev.sender.to_string()
            );
            return;
        }
    } else {
        info!(
            "user attempted invite request too soon: {}",
            ev.sender.as_str()
        );
        return;
    }

    let conf_room = match RoomAliasId::parse(confirmations_room.clone()) {
        Ok(alias_id) => match client.resolve_room_alias(&alias_id).await {
            Ok(res) => match client.get_room(&res.room_id) {
                Some(r) => r,
                None => {
                    error!(
                        "couldn't get room from room id: {confirmations_room} -> {}",
                        res.room_id
                    );
                    return;
                }
            },
            Err(e) => {
                error!("couldn't resolve alias: {} {}", alias_id, e);
                return;
            }
        },
        Err(e) => {
            error!("couldn't parse room alias: {} {}", confirmations_room, e);
            return;
        }
    };

    let sender_display_name: String = match room.get_member(&ev.sender).await {
        Ok(Some(rm)) => match rm.display_name() {
            Some(d) => d.to_owned(),
            None => ev.sender.to_string(),
        },
        _ => ev.sender.to_string(),
    };

    let plain_message = format!(
        r#"{sender_display_name} ({sender_id}) requests members access"#,
        sender_id = ev.sender.to_string(),
    );

    let html_message = format!(
        r#"<a href="{uri}">{sender_display_name}</a>: requests members access"#,
        uri = ev.sender.matrix_to_uri(),
    );

    let msg = RoomMessageEventContent::text_html(plain_message, html_message);

    if let Err(e) = conf_room.send(msg).await {
        error!("error sending confirmation request message: {e}");
        return;
    }
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct InviterConfig {
    pub requests: String,
    pub confirmations: String,
    pub reviewers: Vec<String>,
    pub homeservers_blanket_allow: Vec<String>,
}
