use crate::prelude::*;

use futures::Future;

pub async fn simple_command_wrapper<
    C: de::DeserializeOwned + Clone,
    Fut: Future<Output = anyhow::Result<String>>,
>(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    command_config: C,
    keywords: Vec<String>,
    command: impl Fn(Room, OwnedUserId, String, Vec<String>, C) -> Fut,
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

    let mut iter = text.body.split_whitespace();

    let Some(keyword) = iter.next() else {
        return Ok(());
    };

    if !keywords.contains(&keyword.to_string()) {
        return Ok(());
    };

    let argv: Vec<String> = iter.map(|x| x.to_string()).collect();

    match command(
        room.clone(),
        ev.sender,
        keyword.to_string(),
        argv,
        command_config,
    )
    .await
    {
        Ok(response) => {
            room.send(RoomMessageEventContent::text_plain(response))
                .await?
        }
        Err(e) => {
            room.send(RoomMessageEventContent::text_plain(format!(
                "command retuned error: {e}"
            )))
            .await?
        }
    };

    Ok(())
}

pub(crate) fn modules() -> Vec<ModuleStarter> {
    vec![("demo response", demo_module_starter)]
}

#[derive(Clone, Deserialize)]
pub struct ModuleConfig {
    demo_response: String,
}

fn demo_module_starter(client: &Client, config: &Config) -> anyhow::Result<EventHandlerHandle> {
    let command_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
    Ok(client.add_event_handler(move |ev, room| {
        simple_command_wrapper(
            ev,
            room,
            command_config,
            vec![".demo".to_string()],
            demo_function,
        )
    }))
}

async fn demo_function(
    _room: Room,
    _sender: OwnedUserId,
    _keyword: String,
    _argv: Vec<String>,
    config: ModuleConfig,
) -> anyhow::Result<String> {
    Ok(config.demo_response)
}
