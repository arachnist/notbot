//! Make the bot join and leave rooms as instructed.
//!
//! # Configuration
//!
//! [`ModuleConfig`]
//!
//! ```toml
//! [module."notbot::autojoiner"]
//! homeservers = [
//!     "is-a.cat",
//!     "hackerspace.pl",
//! ]
//! leave_message = "goodbye 😿"
//! ```
//!
//! # Usage
//!
//! The bot responds to chat commands only from bot admins.
//!
//! Keywords:
//! * `join room-name` - attempts to join a room by name. [`join_processor`], [`join_consumer`].
//! * `leave (room-name)` - will leave either the named, or - if name's not present - current room. [`leave_processor`]
//!
//! The bot will also attempt to join rooms when invited, and the room has room_id on one of the allowed homeservers. [`autojoiner`]

use crate::prelude::*;

use tokio::time::{sleep, Duration};

use prometheus::Counter;
use prometheus::{opts, register_counter};

static ROOM_INVITES: LazyLock<Counter> = LazyLock::new(|| {
    register_counter!(opts!("room_invite_events_total", "Number of room invites",)).unwrap()
});

/// Module configuration
#[derive(Clone, Deserialize)]
pub struct ModuleConfig {
    /// List of homeservers to which room ids belong to that the bot will be allowed to join
    pub homeservers: Vec<String>,
    /// Keywords for join requests.
    #[serde(default = "keywords_join")]
    pub keywords_join: Vec<String>,
    /// Keywords for leave requests.
    #[serde(default = "keywords_leave")]
    pub keywords_leave: Vec<String>,
    #[serde(default = "leave_message")]
    /// Message the bot will send to the channel when instructed to leave
    pub leave_message: String,
}

fn keywords_join() -> Vec<String> {
    vec!["join".s()]
}

fn keywords_leave() -> Vec<String> {
    vec!["leave".s(), "part".s()]
}

fn leave_message() -> String {
    String::from("goodbye 😿")
}

pub(crate) fn starter(mx: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering autojoiner");
    let autojoiner_config: ModuleConfig = config.typed_module_config(module_path!())?;
    let autojoiner_handle =
        mx.add_event_handler(move |ev, mx, room| autojoiner(ev, mx, room, autojoiner_config));

    let module_config: ModuleConfig = config.typed_module_config(module_path!())?;

    let (join_tx, join_rx) = mpsc::channel::<ConsumerEvent>(1);
    let join = ModuleInfo {
        name: "join".s(),
        help: "makes the bot join a channel".s(),
        acl: vec![Acl::SpecificUsers(config.admins())],
        trigger: TriggerType::Keyword(module_config.keywords_join.clone()),
        channel: join_tx,
        error_prefix: None,
    };
    tokio::task::spawn(join_consumer(join_rx, mx.clone(), autojoiner_handle));

    let (leave_tx, leave_rx) = mpsc::channel::<ConsumerEvent>(1);
    let leave = ModuleInfo {
        name: "leave".s(),
        help: "makes the bot leave a channel".s(),
        acl: vec![Acl::SpecificUsers(config.admins())],
        trigger: TriggerType::Keyword(module_config.keywords_leave.clone()),
        channel: leave_tx,
        error_prefix: Some("couldn't leave room".s()),
    };
    leave.spawn(leave_rx, module_config.clone(), leave_processor);

    Ok(vec![join, leave])
}

/// Leaves rooms when requested to do so. Will optionally take a room name argument, to leave a different room than current one.
pub async fn leave_processor(event: ConsumerEvent, config: ModuleConfig) -> anyhow::Result<()> {
    let leave_room = if let Some(room_str) = event.args {
        maybe_get_room(&event.room.client(), &room_str).await?
    } else {
        event.room
    };

    leave_room
        .send(RoomMessageEventContent::text_plain(config.leave_message))
        .await?;
    leave_room.leave().await?;
    Ok(())
}

/// Forwards join requests to [`join_processor`] and stops the invite event listener as needed.
pub async fn join_consumer(
    mut rx: mpsc::Receiver<ConsumerEvent>,
    mx: Client,
    autojoiner_handle: EventHandlerHandle,
) -> anyhow::Result<()> {
    loop {
        let event = match rx.recv().await {
            Some(e) => e,
            None => {
                warn!("channel closed");
                info!("stopping the autojoiner");
                mx.remove_event_handler(autojoiner_handle);
                bail!("channel closed");
            }
        };

        if let Err(e) = join_processor(mx.clone(), event.clone()).await {
            error!("couldn't join the room: {e}");
            if let Err(ee) = event
                .room
                .send(RoomMessageEventContent::text_plain(format!(
                    "couldn't join room: {e}"
                )))
                .await
            {
                error!("couldn't send error message: {ee}");
            };
        };
    }
}

/// Processes join requests.
pub async fn join_processor(mx: Client, event: ConsumerEvent) -> anyhow::Result<()> {
    if let Some(room_str) = event.args {
        let room = maybe_get_room(&mx, &room_str).await?;
        info!("joining room: {room_str} {}", room.room_id());
        let mut delay = 2;
        let mut joined = true;

        while let Err(err) = room.join().await {
            // retry autojoin due to synapse sending invites, before the
            // invited user can join for more information see
            // https://github.com/element-hq/synapse/issues/4345
            error!(
                "Failed to join room {} ({err:?}), retrying in {delay}s",
                room.room_id()
            );

            sleep(Duration::from_secs(delay)).await;
            delay *= 2;

            if delay > 3600 {
                error!("Can't join room {} ({err:?})", room.room_id());
                joined = false;
                break;
            }
        }

        trace!("Successfully joined room {}", room.room_id());
        let response = if joined {
            format!("joined {room_str}")
        } else {
            format!("couldn't join {room_str}")
        };
        event
            .room
            .send(RoomMessageEventContent::text_plain(response))
            .await?;
    } else {
        bail!("missing argument: room");
    }

    Ok(())
}

/// Listens for invitation events, and joins the appropriate room if the room id is from one of the permitted homeservers.
pub async fn autojoiner(
    room_member: StrippedRoomMemberEvent,
    client: Client,
    room: Room,
    module_config: ModuleConfig,
) {
    // ignore invites not meant for us
    if room_member.state_key != client.user_id().unwrap() {
        return;
    }

    trace!("getting homeserver name for room");
    let Some(room_homeserver) = &room.room_id().server_name() else {
        return;
    };

    trace!(
        "checking if invite is for a room on permitted homeserver: {:#?}, {:#?}",
        &room_homeserver,
        &module_config.homeservers
    );
    if !module_config
        .homeservers
        .contains(&room_homeserver.to_string())
    {
        return;
    };

    ROOM_INVITES.inc();

    tokio::spawn(async move {
        info!("Autojoining room {}", room.room_id());
        let mut delay = 2;

        while let Err(err) = room.join().await {
            // retry autojoin due to synapse sending invites, before the
            // invited user can join for more information see
            // https://github.com/matrix-org/synapse/issues/4345
            error!(
                "Failed to join room {} ({err:?}), retrying in {delay}s",
                room.room_id()
            );

            sleep(Duration::from_secs(delay)).await;
            delay *= 2;

            if delay > 3600 {
                error!("Can't join room {} ({err:?})", room.room_id());
                break;
            }
        }
        trace!("Successfully joined room {}", room.room_id());
    });
}
