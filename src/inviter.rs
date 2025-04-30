use crate::prelude::*;

use crate::notbottime::{NotBotTime, NOTBOT_EPOCH};

use matrix_sdk::ruma::events::{direct::DirectUserIdentifier, reaction::OriginalSyncReactionEvent};

use axum::{extract::State, response::IntoResponse};
use tower_sessions::Session;

use serde_derive::Deserialize;

#[distributed_slice(MODULE_STARTERS)]
static MODULE_STARTER: ModuleStarter = (module_path!(), module_starter);

fn module_starter(client: &Client, config: &Config) -> anyhow::Result<EventHandlerHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
    Ok(client.add_event_handler(move |ev, room, client| {
        module_entrypoint(ev, room, client, module_config)
    }))
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct ModuleConfig {
    pub requests: String,
    pub approvers: Vec<String>,
    pub homeserver_selfservice_allow: String,
    pub invite_to: Vec<String>,
}

async fn module_entrypoint(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    client: Client,
    config: ModuleConfig,
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
    client.add_event_handler(move |ev, room, client, handle| {
        reaction_listener(ev, room, client, handle, config, evid, evsender)
    });
}

async fn reaction_listener(
    ev: OriginalSyncReactionEvent,
    room: Room,
    client: Client,
    handle: EventHandlerHandle,
    config: ModuleConfig,
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

    client.remove_event_handler(handle);

    let note = match inviter(client, orig_ev_sender.clone(), config.invite_to).await {
        Err(e) => {
            error!("sending invites failed: {}", e);
            ", but sending invites may have failed. ask staff for help"
        }
        _ => "",
    };

    let requester_display_name: String = match room.get_member(&orig_ev_sender).await {
        Ok(Some(rm)) => match rm.display_name() {
            Some(d) => d.to_owned(),
            None => orig_ev_sender.to_string(),
        },
        _ => orig_ev_sender.to_string(),
    };

    let plain_message =
        format!(r#"{requester_display_name}: your invitation request has been approved{note}"#,);

    let html_message = format!(
        r#"<a href="{uri}">{requester_display_name}</a>: your invitation request has been approved{note}"#,
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
        trace!("inviting to {}", maybe_room.clone());
        let room_id: OwnedRoomId = match maybe_room.clone().try_into() {
            Ok(r) => r,
            Err(_) => {
                let alias_id = OwnedRoomAliasId::try_from(maybe_room.clone())?;

                client.resolve_room_alias(&alias_id).await?.room_id
            }
        };

        let room = match client.get_room(&room_id) {
            Some(r) => r,
            None => continue,
        };

        match room.invite_user_by_id(&invitee).await {
            Ok(_) => trace!("invitation ok: {maybe_room}"),
            Err(e) => error!("some error occured while inviting to {maybe_room}: {e}"),
        };
    }
    Ok(())
}

pub(crate) async fn web_inviter(
    State(app_state): State<WebAppState>,
    session: Session,
) -> Result<impl IntoResponse, (axum::http::StatusCode, &'static str)> {
    async {
        let module_config: ModuleConfig = app_state
            .bot_config
            .module_config_value(module_path!())?
            .try_into()?;

        let user_data: OauthUserInfo = session.get("user.data").await.unwrap().unwrap();

        let user_id_str = format!(
            "@{username}:{homeserver}",
            username = user_data.sub,
            homeserver = module_config.homeserver_selfservice_allow
        );

        let bdui: Box<DirectUserIdentifier> = From::<String>::from(user_id_str);
        let user_id: OwnedUserId = match bdui.as_user_id() {
            None => return Err(anyhow::Error::msg("couldn't get user id")),
            Some(u) => u.to_owned(),
        };

        inviter(app_state.mx, user_id, module_config.invite_to).await?;

        Ok(())
    }
    .await
    .map_err(|err: anyhow::Error| {
        error!("Failed to invite user: {:?}", err);
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to invite user",
        )
    })?;

    Ok(())
}
