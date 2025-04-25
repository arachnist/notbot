use bytes::Bytes;
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, SystemTime};

use crate::{
    notbottime::{NotBotTime, NOTBOT_EPOCH},
    Config, ModuleStarter, WorkerStarter, MODULE_STARTERS, WORKERS,
};

use std::net::SocketAddr;

use tracing::{error, info, trace, warn};

use http_body_util::Full;
use hyper::{body::Incoming, server::conn::http1, service::Service, Error, Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::task::AbortHandle;

use linkme::distributed_slice;
use matrix_sdk::{
    event_handler::EventHandlerHandle,
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

#[distributed_slice(WORKERS)]
static WORKER_STARTER: WorkerStarter = (module_path!(), worker_starter);

fn worker_starter(mx: &Client, config: &Config) -> anyhow::Result<AbortHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
    let worker = tokio::task::spawn(worker_entrypoint(mx.clone(), module_config));
    Ok(worker.abort_handle())
}

#[derive(Debug, Clone)]
struct SelfInvite {
    mx: Client,
}

impl Service<Request<Incoming>> for SelfInvite {
    type Response = Response<Full<Bytes>>;
    type Error = hyper::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        fn mk_response(s: String) -> Result<Response<Full<Bytes>>, hyper::Error> {
            Ok(Response::builder().body(Full::new(Bytes::from(s))).unwrap())
        }

        let mut resp = "".to_string();
        for (k, v) in req.headers().iter() {
            resp.push_str(format!("{k}: {v:?}\n").as_str());
        }

        let res = match req.uri().path() {
            &_ => mk_response(resp),
        };

        Box::pin(async { res })
    }
}

async fn worker_entrypoint(mx: Client, module_config: ModuleConfig) -> anyhow::Result<()> {
    let addr: SocketAddr = module_config.listen_address.parse()?;
    let listener = TcpListener::bind(addr).await?;
    info!("listening on {addr}");

    let self_invite = SelfInvite { mx };
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                warn!("accepting socket failed: {e}");
                continue;
            }
        };

        let io = TokioIo::new(stream);

        let inviter_web = self_invite.clone();

        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .serve_connection(io, inviter_web)
                .await
            {
                error!("Failed to serve connection: {:?}", err);
            }
        });
    }
}

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
    pub homeservers_selfservice_allow: Vec<String>,
    pub invite_to: Vec<String>,
    pub listen_address: String,
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
