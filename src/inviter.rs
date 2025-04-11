use std::time::{Duration, SystemTime};

use crate::{
    notbottime::{NotBotTime, NOTBOT_EPOCH},
    Config, MODULES,
};

use tracing::{error, info, trace};

use linkme::distributed_slice;
use matrix_sdk::{
    ruma::{
        events::{
            reaction::OriginalSyncReactionEvent,
            room::message::{MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent},
            Mentions,
        },
        OwnedEventId, OwnedRoomAliasId, OwnedRoomId, OwnedUserId,
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
            error!("Couldn't load inviter config: {e}");
            return;
        }
    };

    c.add_event_handler(move |ev, room, client| inviter_listener(ev, room, client, inviter_config));
}

async fn inviter_listener(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    client: Client,
    config: InviterConfig,
) {
    if let Some(alias) = room.canonical_alias() {
        if config.requests != alias.as_str() {
            return;
        };
    } else {
        return;
    };

    let MessageType::Text(text) = ev.content.msgtype else {
        return;
    };

    if !text.body.trim().starts_with(".invite") {
        return;
    };

    if ev.sender == client.user_id().unwrap() {
        return;
    };

    info!("invitation request from: {}", ev.sender);
    let prefixed_sender = "inviter:".to_owned() + ev.sender.as_str();
    let store = client.state_store();

    let next_allowed_attempt = match store.get_custom_value(prefixed_sender.as_bytes()).await {
        Err(e) => {
            error!("error fetching next allowed attempt time: {e}");
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

    let evid = ev.event_id.clone();
    let evsender = ev.sender.clone();

    trace!("adding reaction event handler for current event");
    client.add_event_handler(move |ev, room, client| {
        reaction_listener(ev, room, client, config, evid, evsender)
    });

    return;
}

async fn reaction_listener(
    ev: OriginalSyncReactionEvent,
    room: Room,
    client: Client,
    config: InviterConfig,
    orig_ev_id: OwnedEventId,
    orig_ev_sender: OwnedUserId,
) {
    if ev.content.relates_to.event_id != orig_ev_id {
        return;
    };

    if !ev.content.relates_to.key.chars().any(|c| c == '\u{1f44d}') {
        return;
    };

    if !config.approvers.contains(&ev.sender.to_string()) {
        return;
    }

    if let Err(e) = inviter(client, orig_ev_sender.clone(), config.invite_to).await {
        error!("sending invites failed: {}", e);
        if let Err(se) = room
            .send(RoomMessageEventContent::text_plain(format!(
                "sorry, sending invites failed: {}",
                e
            )))
            .await
        {
            error!("sending error notification failed: {}", se);
        };
    };

    let requester_display_name: String = match room.get_member(&orig_ev_sender).await {
        Ok(Some(rm)) => match rm.display_name() {
            Some(d) => d.to_owned(),
            None => orig_ev_sender.to_string(),
        },
        _ => orig_ev_sender.to_string(),
    };

    let plain_message =
        format!(r#"{requester_display_name}: your invitation request has been approved"#,);

    let html_message = format!(
        r#"<a href="{uri}">{requester_display_name}</a>: your invitation request has been approved"#,
        uri = orig_ev_sender.matrix_to_uri(),
    );

    let msg = RoomMessageEventContent::text_html(plain_message, html_message)
        .add_mentions(Mentions::with_user_ids(vec![orig_ev_sender]));

    match room.send(msg).await {
        Err(e) => error!("error sending confirmation request message: {e}"),
        Ok(r) => info!("request event id: {}", r.event_id),
    }
}

async fn inviter(client: Client, invitee: OwnedUserId, rooms: Vec<String>) -> anyhow::Result<()> {
    trace!("rooms to invite to: {:#?}", rooms);
    for maybe_room in rooms {
        let room_id: OwnedRoomId = match maybe_room.clone().try_into() {
            Ok(r) => r,
            Err(_) => {
                let alias_id = OwnedRoomAliasId::try_from(maybe_room)?;

                client.resolve_room_alias(&alias_id).await?.room_id
            }
        };

        let room = match client.get_room(&room_id) {
            Some(r) => r,
            None => continue,
        };

        room.invite_user_by_id(&invitee).await?
    }
    Ok(())
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct InviterConfig {
    pub requests: String,
    pub approvers: Vec<String>,
    pub homeservers_blanket_allow: Vec<String>,
    pub invite_to: Vec<String>,
}
