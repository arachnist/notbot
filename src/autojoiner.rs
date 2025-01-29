use linkme::distributed_slice;

use matrix_sdk::{
    event_handler::Ctx, ruma::events::room::member::StrippedRoomMemberEvent, Client, Room,
};

use tokio::time::{sleep, Duration};

use crate::{Config, MODULES};

#[distributed_slice(MODULES)]
static AUTOJOINER: fn(&Client) = callback_registrar;

fn callback_registrar(c: &Client) {
    tracing::info!("registering autojoiner");
    c.add_event_handler(autojoin_on_invites);
}

async fn autojoin_on_invites(
    room_member: StrippedRoomMemberEvent,
    client: Client,
    room: Room,
    Ctx(config): Ctx<Config>,
) {
    // ignore invites not meant for us
    if room_member.state_key != client.user_id().unwrap() {
        return;
    }

    tracing::debug!("getting homeserver name for room");
    let Some(room_homeserver) = &room.room_id().server_name() else {
        return;
    };

    tracing::debug!("checking if invite is for a room on permitted homeserver");
    let Some(_) = config.module["autojoiner"]["HomeServers"].get(&room_homeserver.to_string())
    else {
        tracing::info!(
            "ignoring invite from {:#?} {:#?}",
            room_homeserver,
            room_member
        );
        return;
    };

    tokio::spawn(async move {
        tracing::info!("Autojoining room {}", room.room_id());
        let mut delay = 2;

        while let Err(err) = room.join().await {
            // retry autojoin due to synapse sending invites, before the
            // invited user can join for more information see
            // https://github.com/matrix-org/synapse/issues/4345
            tracing::error!(
                "Failed to join room {} ({err:?}), retrying in {delay}s",
                room.room_id()
            );

            sleep(Duration::from_secs(delay)).await;
            delay *= 2;

            if delay > 3600 {
                tracing::error!("Can't join room {} ({err:?})", room.room_id());
                break;
            }
        }
        tracing::info!("Successfully joined room {}", room.room_id());
    });
}
