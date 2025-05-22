use crate::prelude::*;

fn default_keywords() -> Vec<String> {
    vec!["sage".s()]
}

#[derive(Clone, Deserialize)]
pub struct ModuleConfig {
    pub protected: Vec<String>,
    pub protected_reason: String,
    pub reason: String,
    pub no_target_response: String,
    #[serde(default = "default_keywords")]
    pub keywords: Vec<String>,
}

pub fn starter(_: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering modules");
    let module_config: ModuleConfig = config.typed_module_config(module_path!())?;

    let (tx, rx) = mpsc::channel::<ConsumerEvent>(1);
    let sage = ModuleInfo {
        name: "sage".s(),
        help: "lol, lmao".s(),
        acl: vec![],
        trigger: TriggerType::Keyword(module_config.keywords.clone()),
        channel: tx,
        error_prefix: Some("failed to remove them".s()),
    };
    sage.spawn(rx, module_config, processor);

    Ok(vec![sage])
}

async fn processor(event: ConsumerEvent, config: ModuleConfig) -> anyhow::Result<()> {
    let Ok(target) = (|| -> anyhow::Result<OwnedUserId> {
        if let Some(mut mentions_set) = event.ev.content.mentions {
            if let Some(userid) = mentions_set.user_ids.pop_first() {
                return Ok(userid);
            }
        };

        if let Some(target) = event.args {
            if let Ok(userid) = UserId::parse(target) {
                return Ok(userid);
            };
        };

        Err(anyhow!("no target found"))
    })() else {
        event
            .room
            .send(RoomMessageEventContent::text_plain(
                config.no_target_response,
            ))
            .await?;
        return Ok(());
    };

    if config.protected.contains(&target.to_string()) {
        event
            .room
            .kick_user(&event.sender, Some(&config.protected_reason))
            .await?;
        return Ok(());
    };

    event.room.kick_user(&target, Some(&config.reason)).await?;

    Ok(())
}
