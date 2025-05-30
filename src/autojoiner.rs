//! Make the bot join and leave rooms as instructed.
//!
//! # Configuration
//!
//! [`ModuleConfig`]
//!
//! ```toml
//! [module."notbot::autojoiner"]
//! leave_message = "goodbye 😿"
//! ```
//!
//! # Usage
//!
//! The bot responds to chat commands only from bot admins.
//!
//! Keywords:
//! * `join room-name` - attempts to join a room by name. [`join_processor`], [`join_consumer`].
//! * `leave [room-name]` - will leave either the named, or - if name's not present - current room. [`leave_processor`]

use crate::prelude::*;

use tokio::time::{Duration, sleep};

/// Module configuration
#[derive(Clone, Deserialize)]
pub struct ModuleConfig {
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

pub(crate) fn starter(_: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering autojoiner");

    let module_config: ModuleConfig = config.typed_module_config(module_path!())?;

    Ok(vec![
        ModuleInfo::new(
            "join",
            "makes the bot join a channel",
            vec![Acl::SpecificUsers(config.admins())],
            TriggerType::Keyword(module_config.keywords_join.clone()),
            None,
            module_config.clone(),
            join_processor,
        ),
        ModuleInfo::new(
            "leave",
            "makes the bot leave a channel",
            vec![Acl::SpecificUsers(config.admins())],
            TriggerType::Keyword(module_config.keywords_leave.clone()),
            Some("couldn't leave room"),
            module_config,
            leave_processor,
        ),
    ])
}

/// Leaves rooms when requested to do so. Will optionally take a room name argument, to leave a different room than current one.
///
/// # Errors
/// Will return `Err` if:
/// * can't resolve room provided as argument
/// * sending goodbye message fails
/// * leaving the room fails
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

/// Processes join requests.
///
/// # Errors
/// Will return `Err` if:
/// * no argument is provided
/// * argument doesn't parse as room
/// * joining room fails.
pub async fn join_processor(event: ConsumerEvent, _: ModuleConfig) -> anyhow::Result<()> {
    let Some(room_str) = event.args else {
        bail!("missing argument: room");
    };

    let room = maybe_get_room(&event.room.client(), &room_str).await?;
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
        bail!("couldn't join {room_str}")
    };
    event
        .room
        .send(RoomMessageEventContent::text_plain(response))
        .await?;

    Ok(())
}
