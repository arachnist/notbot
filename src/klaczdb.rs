use crate::prelude::*;

#[derive(Clone, Deserialize)]
pub struct ModuleConfig {
    pub handle: String,
}

pub(crate) fn modules() -> Vec<ModuleStarter> {
    vec![("notbot::oodkb::add", module_starter_add)]
}

fn module_starter_add(client: &Client, config: &Config) -> anyhow::Result<EventHandlerHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
    Ok(client.add_event_handler(move |ev, room| add(ev, room, module_config)))
}

async fn add(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    config: ModuleConfig,
) -> anyhow::Result<()> {
    if room.state() != RoomState::Joined {
        return Ok(());
    };

    if ev.sender == room.client().user_id().unwrap() {
        return Ok(());
    };

    let MessageType::Text(text) = ev.content.msgtype else {
        return Ok(());
    };

    let mut args = text.body.splitn(3, [' ', '\n']);

    let Some(keyword) = args.next() else {
        return Ok(());
    };

    if keyword != ".add" {
        return Ok(());
    };
    
    let Some(term) = args.next() else {
        room.send(RoomMessageEventContent::text_plain("missing arguments: term, definition")).await?;
        return Err(anyhow::Error::msg("missing arguments"));
    };
    
    let Some(definition) = args.next() else {
        room.send(RoomMessageEventContent::text_plain("missing arguments: definition")).await?;
        return Err(anyhow::Error::msg("missing arguments"));
    };

    room.send(RoomMessageEventContent::text_plain(format!("would add: term: {term}: definition: {definition}"))).await?;
    Ok(())
}
