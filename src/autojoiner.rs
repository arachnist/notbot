use linkme::distributed_slice;

use matrix_sdk::{ruma::events::room::member::StrippedRoomMemberEvent, Client, Room};

use tokio::time::{sleep, Duration};
use tracing::{debug, error, info, trace};

use crate::{Config, MODULES};

#[distributed_slice(MODULES)]
static AUTOJOINER: fn(&Client, &Config) = callback_registrar;

fn callback_registrar(c: &Client, config: &Config) {
    info!("registering autojoiner");

    let homeservers: Vec<String> = config.module["autojoiner"]["homeservers"]
        .clone()
        .try_into()
        .expect("list of allowed homeservers needs to be defined");
    debug!("homeservers: {:#?}", &homeservers);
    c.add_event_handler(move |ev, client, room| autojoin_on_invites(ev, client, room, homeservers));
}

async fn autojoin_on_invites(
    room_member: StrippedRoomMemberEvent,
    client: Client,
    room: Room,
    homeservers: Vec<String>,
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
        &homeservers
    );
    if !homeservers.contains(&room_homeserver.to_string()) {
        return;
    };

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
