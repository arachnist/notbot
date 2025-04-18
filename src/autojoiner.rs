use crate::{Config, ModuleStarter, MODULE_STARTERS};

use linkme::distributed_slice;

use matrix_sdk::{
    event_handler::EventHandlerHandle, ruma::events::room::member::StrippedRoomMemberEvent, Client,
    Room,
};

use serde_derive::Deserialize;
use tokio::time::{sleep, Duration};
use tracing::{error, info, trace};

use lazy_static::lazy_static;
use prometheus::Counter;
use prometheus::{opts, register_counter};

lazy_static! {
    static ref ROOM_INVITES: Counter =
        register_counter!(opts!("room_invite_events_total", "Number of room invites",)).unwrap();
}

#[distributed_slice(MODULE_STARTERS)]
static MODULE_STARTER: ModuleStarter = (module_path!(), module_starter);

fn module_starter(client: &Client, config: &Config) -> anyhow::Result<EventHandlerHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
    Ok(client.add_event_handler(move |ev, client, room| {
        module_entrypoint(ev, client, room, module_config)
    }))
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct ModuleConfig {
    pub homeservers: Vec<String>,
}

async fn module_entrypoint(
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
