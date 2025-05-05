use crate::prelude::*;

#[derive(Clone, Deserialize)]
pub struct ModuleConfig {
    pub protected: Vec<String>,
    pub protected_reason: String,
    pub reason: String,
    pub no_target_response: String,
}

pub(crate) fn starter(_: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering modules");
    let mut modules: Vec<ModuleInfo> = vec![];

    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;

    let (tx, rx) = mpsc::channel::<ConsumerEvent>(1);
    tokio::task::spawn(consumer(rx, module_config));
    let at = ModuleInfo {
        name: "sage".s(),
        help: "lol, lmao".s(),
        acl: vec![],
        trigger: TriggerType::Keyword(vec!["sage".s()]),
        channel: Some(tx),
    };
    modules.push(at);

    Ok(modules)
}

async fn consumer(
    mut rx: mpsc::Receiver<ConsumerEvent>,
    config: ModuleConfig,
) -> anyhow::Result<()> {
    loop {
        let event = match rx.recv().await {
            Some(e) => e,
            None => {
                error!("channel closed, goodbye! :(");
                bail!("channel closed");
            }
        };

        if let Err(e) = processor(event.clone(), config.clone()).await {
            if let Err(e) = event
                .room
                .send(RoomMessageEventContent::text_plain(format!(
                    "failed removing someone: {e}"
                )))
                .await
            {
                error!("error while sending response: {e}");
            };
        }
    }
}

async fn processor(event: ConsumerEvent, config: ModuleConfig) -> anyhow::Result<()> {
    let target = match (|| -> anyhow::Result<OwnedUserId> {
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
    })() {
        Ok(u) => u,
        Err(_) => {
            event
                .room
                .send(RoomMessageEventContent::text_plain(
                    config.no_target_response,
                ))
                .await?;
            return Ok(());
        }
    };

    if config.protected.contains(&target.to_string()) {
        event
            .room
            .kick_user(&event.sender, Some(&config.protected_reason))
            .await?;
        return Ok(());
    } else {
        event.room.kick_user(&target, Some(&config.reason)).await?;
    };

    Ok(())
}
