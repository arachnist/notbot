use crate::{Config, ModuleStarter, MODULE_STARTERS};

use tracing::{error, info, trace};

use linkme::distributed_slice;
use matrix_sdk::{
    event_handler::EventHandlerHandle,
    ruma::events::reaction::OriginalSyncReactionEvent,
    ruma::events::room::message::{
        MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
    },
    Client, Room,
};

#[distributed_slice(MODULE_STARTERS)]
static MODULE_STARTER: ModuleStarter = (module_path!(), module_starter);

fn module_starter(client: &Client, _: &Config) -> anyhow::Result<EventHandlerHandle> {
    Ok(client.add_event_handler(shenanigans))
}

#[distributed_slice(MODULE_STARTERS)]
static MODULE_STARTER_REACTION_DUMPER: ModuleStarter =
    (module_path!(), module_starter_reaction_dumper);

fn module_starter_reaction_dumper(
    client: &Client,
    _: &Config,
) -> anyhow::Result<EventHandlerHandle> {
    Ok(client.add_event_handler(reaction_dumper))
}

async fn shenanigans(ev: OriginalSyncRoomMessageEvent, room: Room) {
    trace!("in shenanigans");

    trace!("checking message type");
    let MessageType::Text(text) = ev.content.msgtype else {
        return;
    };

    if !text.body.starts_with(".shenanigans") {
        return;
    }

    if let Err(se) = room
        .send(RoomMessageEventContent::text_html(
            "!irc this message requires Matrix Gold subscription",
            "this message requires IRC+ subscription",
        ))
        .await
    {
        error!("error sending response: {se}");
    };
    return;
}

async fn reaction_dumper(ev: OriginalSyncReactionEvent, _room: Room) {
    info!(
        "compares: {}",
        ev.content.relates_to.key.chars().any(|c| c == '\u{1f44d}')
    );

    const THUMBS_UP_PREFIX: [u8; 4] = [240, 159, 145, 141];
    if ev
        .content
        .relates_to
        .key
        .as_bytes()
        .starts_with(&THUMBS_UP_PREFIX)
    {
        info!("bytes reaction matched: {:#?}", ev.content.relates_to.key);
    };
}
