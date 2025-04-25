use crate::{Config, ModuleStarter, MODULE_STARTERS};

use tracing::{error, trace};

use linkme::distributed_slice;
use matrix_sdk::{
    event_handler::EventHandlerHandle,
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
            "this message requires Matrix Gold subscription",
            "<br/>this message requires IRC+ subscription",
        ))
        .await
    {
        error!("error sending response: {se}");
    };
}
