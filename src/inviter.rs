//! Ease the process of inviting new members to member-only rooms and spaces
//!
//! # Configuration
//!
//! [`ModuleConfig`]
//!
//! ```toml
//! [module."notbot::inviter"]
//! requests = [ "#general:example.org" ]
//! approvers = [
//!     "@bar:example.com",
//!     "@foo:hackerspace.pl",
//!     "@baz:hackerspace.pl",
//! ]
//! invite_to = [
//!     "!LPEcDKoGvnCdhRSTJW:example.org",
//!     "#bottest:example.org"
//! ]
//! homeserver_selfservice_allow = "example.org"
//! keywords = [ "invite" ]
//! ```
//!
//! # Usage
//!
//! New user is expected to write ".invite" on a public channel; [`invite_request`]. A trusted user is then expected to
//! confirm the invitation request by reacting with 👍 emoji; [`reaction_listener`]. Color variants *should* work, but are
//! not guaranteed to.
//!
//! There is also a prototype of the self-service through sso flow implemented, [`web_inviter`], registered under
//! `/mx/inviter/invite url`, that will attempt to extract username from the OIDC claim and invite
//! a `@username:allowed-homeserver.org` user.
//!
//! # Future
//!
//! * Support for processing room knocking events as requests.
//! * A better self-service flow once changes in relevant parts of the rest of hackerspace infrastructure are done.

use crate::prelude::*;

use crate::notbottime::{NOTBOT_EPOCH, NotBotTime};

use matrix_sdk::ruma::events::reaction::OriginalSyncReactionEvent;

use axum::{extract::State, response::IntoResponse};
use axum_oidc::OidcClaims;

fn default_keywords() -> Vec<String> {
    vec!["invite".s()]
}

/// Configuration structure for the module.
#[derive(Clone, Deserialize)]
pub struct ModuleConfig {
    /// Rooms on which we permit requests.
    pub requests: Vec<String>,
    /// Trusted users that are allowed to approve invitation requests.
    pub approvers: Vec<String>,
    /// Known homeserver address for which we allow users to invite themselves using bot web interface.
    pub homeserver_selfservice_allow: String,
    /// Rooms and spaces that the new user will be invited to.
    pub invite_to: Vec<String>,
    /// keywords the bot will listen for invitation requests.
    #[serde(default = "default_keywords")]
    pub keywords: Vec<String>,
}

pub(crate) fn starter(_: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering modules");
    let module_config: ModuleConfig = config.typed_module_config(module_path!())?;

    let (tx, rx) = mpsc::channel::<ConsumerEvent>(1);
    let inviter = ModuleInfo {
        name: "inviter".s(),
        help: "processes invite requests to hackerspace matrix rooms and spaces".s(),
        acl: vec![Acl::Room(module_config.requests.clone())],
        trigger: TriggerType::Keyword(module_config.keywords.clone()),
        channel: tx,
        error_prefix: Some("couldn't process invite request".s()),
    };
    inviter.spawn(rx, module_config, invite_request);

    Ok(vec![inviter])
}

/// Listens for invite requests from new hackerspace members, and attempts to invite them to
/// configured rooms and spaces, and starts the [`reaction_listener`] as needed.
///
/// # Errors
/// Will return `Err` if any of the following fails:
/// * checking when the user will be allowed to attempt an invite
/// *
pub async fn invite_request(event: ConsumerEvent, config: ModuleConfig) -> anyhow::Result<()> {
    info!("invitation request from: {}", event.sender);
    let prefixed_sender = "inviter:".to_owned() + event.sender.as_str();
    let client = event.room.client();
    let store = client.state_store();

    let next_allowed_attempt = match store.get_custom_value(prefixed_sender.as_bytes()).await {
        Err(e) => {
            bail!("error fetching next allowed attempt time: {e}");
        }
        Ok(maybe_result) => maybe_result.map_or(NOTBOT_EPOCH, std::convert::Into::into),
    };

    if NotBotTime::now() > next_allowed_attempt {
        let next_allowed_attempt =
            NotBotTime(SystemTime::now() + Duration::from_secs(60 * 60 * 24 * 7));
        store
            .set_custom_value_no_read(prefixed_sender.as_bytes(), next_allowed_attempt.into())
            .await?;
    } else {
        info!(
            "user attempted invite request too soon: {}",
            event.sender.as_str()
        );
        return Ok(());
    }

    let evid = event.ev.event_id.clone();
    let evsender = event.sender.clone();
    client.add_event_handler(move |ev, room, client, handle| {
        reaction_listener(ev, room, client, handle, config.clone(), evid, evsender)
    });

    Ok(())
}

/// Listens for `👍` emoji reactions from approved trusted users on the invitation request message, and attempts to invite the user to configured channels.
pub async fn reaction_listener(
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
        Ok(Some(rm)) => rm.display_name().map_or_else(
            || orig_ev_sender.to_string(),
            std::borrow::ToOwned::to_owned,
        ),
        _ => orig_ev_sender.to_string(),
    };

    let plain_message =
        format!(r"{requester_display_name}: your invitation request has been approved{note}",);

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
        let room_id: OwnedRoomId = if let Ok(r) = maybe_room.clone().try_into() {
            r
        } else {
            let alias_id = OwnedRoomAliasId::try_from(maybe_room.clone())?;

            client.resolve_room_alias(&alias_id).await?.room_id
        };

        let Some(room) = client.get_room(&room_id) else {
            continue;
        };

        match room.invite_user_by_id(&invitee).await {
            Ok(()) => trace!("invitation ok: {maybe_room}"),
            Err(e) => error!("some error occured while inviting to {maybe_room}: {e}"),
        };
    }
    Ok(())
}

/// Endpoint for processing invites from the web interface.
///
/// # Errors
/// Will return `Err` if sso returns missing or invalid matrix user id.
pub async fn web_inviter(
    State(app_state): State<WebAppState>,
    OidcClaims(claims): OidcClaims<HswawAdditionalClaims>,
) -> Result<impl IntoResponse, (axum::http::StatusCode, &'static str)> {
    async {
        let module_config: ModuleConfig = app_state.config.typed_module_config(module_path!())?;

        let user_id: OwnedUserId = UserId::parse(
            claims
                .additional_claims()
                .matrix_user
                .clone()
                .ok_or_else(|| anyhow!("matrix user missing in claim"))?,
        )?;

        tokio::spawn(inviter(app_state.mx, user_id, module_config.invite_to));
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
