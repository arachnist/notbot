use crate::prelude::*;

pub(crate) fn modules() -> Vec<ModuleStarter> {
    vec![(module_path!(), module_starter)]
}

fn module_starter(client: &Client, config: &Config) -> anyhow::Result<EventHandlerHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
    Ok(client.add_event_handler(move |ev, room| module_entrypoint(ev, room, module_config)))
}

#[derive(Clone, Deserialize)]
pub struct ModuleConfig {
    pub protected: Vec<String>,
    pub protected_reason: String,
    pub reason: String,
    pub no_target_response: String,
}

async fn module_entrypoint(
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

    let mut args = text.body.split_whitespace();

    let Some(keyword) = args.next() else {
        return Ok(());
    };

    if keyword != ".sage" {
        return Ok(());
    };

    let target = match (|| -> anyhow::Result<OwnedUserId> {
        if let Some(mut mentions_set) = ev.content.mentions {
            if let Some(userid) = mentions_set.user_ids.pop_first() {
                return Ok(userid);
            }
        };

        if let Some(target) = args.next() {
            if let Ok(userid) = UserId::parse(target) {
                return Ok(userid);
            };
        };

        Err(anyhow!("no target found"))
    })() {
        Ok(u) => u,
        Err(_) => {
            room.send(RoomMessageEventContent::text_plain(
                config.no_target_response,
            ))
            .await?;
            return Ok(());
        }
    };

    if config.protected.contains(&target.to_string()) {
        room.kick_user(&ev.sender, Some(&config.protected_reason))
            .await?;
        return Ok(());
    } else {
        room.kick_user(&target, Some(&config.reason)).await?;
    };

    Ok(())
}
