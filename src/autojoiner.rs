use linkme::distributed_slice;

use matrix_sdk::{
    Client,
    Room,
    ruma::events::room::member::StrippedRoomMemberEvent,
    event_handler::Ctx,
};

use tokio::time::{sleep, Duration};

use crate::{CALLBACKS, Config};

#[distributed_slice(CALLBACKS)]
static AUTOJOINER: fn(&Client) = callback_registrar;

fn callback_registrar(c: &Client) {
    tracing::info!("registering autojoiner");
    c.add_event_handler(autojoin_on_invites);
}

async fn autojoin_on_invites(
    room_member: StrippedRoomMemberEvent,
    client: Client,
    room: Room,
    Ctx(config): Ctx<Config>
) {
    // ignore invites not meant for us
    if room_member.state_key != client.user_id().unwrap() {
        return;
    }

    tracing::debug!("extracting homeserver list from config");
    let allowed_homeservers: Vec<String> = config.module["autojoiner"]["homeservers"].clone().try_into().unwrap();
    let room_homeserver = &room.room_id().server_name().unwrap().to_string();

    tracing::debug!("checking if invite is for a room on permitted homeserver: {:#?}, {:#?}", &room.room_id(), allowed_homeservers);
    if !allowed_homeservers.contains(room_homeserver) {
        tracing::info!("ignoring invite from {:#?} {:#?}", room_homeserver, room_member);
        return
    }

    tokio::spawn(async move {
        tracing::info!("Autojoining room {}", room.room_id());
        let mut delay = 2;

        while let Err(err) = room.join().await {
            // retry autojoin due to synapse sending invites, before the
            // invited user can join for more information see
            // https://github.com/matrix-org/synapse/issues/4345
            tracing::error!("Failed to join room {} ({err:?}), retrying in {delay}s", room.room_id());

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
