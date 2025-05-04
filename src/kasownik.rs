use crate::prelude::*;

use crate::notbottime::NotBotTime;

#[derive(Clone, Deserialize)]
pub struct ModuleConfig {
    pub url_template: String,
    pub nag_channels: Vec<String>,
    pub nag_late_fees: i64,
    pub due_others_allowed: Vec<String>,
}

pub(crate) fn starter(mx: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering modules");
    let mut modules: Vec<ModuleInfo> = vec![];

    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;

    let (duetx, duerx) = mpsc::channel::<ConsumerEvent>(1);
    tokio::task::spawn(due_consumer(duerx, module_config.clone()));
    let due = ModuleInfo {
        name: "due".s(),
        help: "checks how many membership fees a member is missing".s(),
        acl: vec![Acl::Room(module_config.due_others_allowed.clone())],
        trigger: TriggerType::Keyword(vec!["due".s()]),
        channel: Some(duetx),
    };
    modules.push(due);

    let (duemetx, duemerx) = mpsc::channel::<ConsumerEvent>(1);
    tokio::task::spawn(due_me_consumer(duemerx, module_config.clone()));
    let dueme = ModuleInfo {
        name: "due-me".s(),
        help: "checks how many membership fees you are missing".s(),
        acl: vec![],
        trigger: TriggerType::Keyword(vec!["due-me".s(), "dueme".s()]),
        channel: Some(duemetx),
    };
    modules.push(dueme);

    Ok(modules)
}

async fn due_consumer(
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

        if let Err(e) = due(event.clone(), config.clone()).await {
            if let Err(e) = event
                .room
                .send(RoomMessageEventContent::text_plain(format!(
                    "error getting presence status: {e}"
                )))
                .await
            {
                error!("error while sending presence status: {e}");
            };
        }
    }
}

async fn due(event: ConsumerEvent, config: ModuleConfig) -> anyhow::Result<()> {
    use MembershipStatus::*;

    if event.args.is_none() {
        event
            .room
            .send(RoomMessageEventContent::text_plain(format!(
                "missing argument: member"
            )))
            .await?;
    };

    let target = {
        let mut candidate: Option<OwnedUserId> = None;
        if let Some(mut mentions_set) = event.ev.content.mentions {
            trace!("mentions: {mentions_set:#?}");
            if let Some(userid) = mentions_set.user_ids.pop_first() {
                candidate = Some(userid)
            }
        }

        if candidate.is_none() {
            let remainder = event.args.unwrap();
            let mut args = remainder.split_whitespace();
            if let Some(plain_target) = args.next() {
                let maybe_mxid = format!("@{plain_target}:hackerspace.pl");
                candidate = UserId::parse(maybe_mxid).ok();
            };
        };

        // we tried our best
        if candidate.is_none() {
            event
                .room
                .send(RoomMessageEventContent::text_plain(format!(
                    "member argument missing or couldn't parse"
                )))
                .await?;
            return Ok(());
        };

        candidate.unwrap()
    };

    let status = membership_status(target.clone()).await?;
    let member = target.localpart();

    let response = match membership_status(target.clone()).await? {
        NotAMember => "not a member".s(),
        Stoned => "stoned".s(),
        Inactive => "not currently a member".s(),
        Active(months) => match months {
            i64::MIN..0 => format!("{member} is {} months ahead. Cool!", 0 - months),
            0 => format!("{member} has paid all their membership fees."),
            1 => format!("{member} needs to pay one membership fee."),
            2..=i64::MAX => format!("{member} needs to pay {months} membership fees."),
        },
    };

    event
        .room
        .send(RoomMessageEventContent::text_plain(response))
        .await?;

    Ok(())
}

async fn due_me_consumer(
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

        trace!("we got the event!");

        if let Err(e) = new_due_me(event.clone(), config.clone()).await {
            if let Err(e) = event
                .room
                .send(RoomMessageEventContent::text_plain(format!(
                    "error getting presence status: {e}"
                )))
                .await
            {
                error!("error while sending presence status: {e}");
            };
        }
    }
}

async fn new_due_me(event: ConsumerEvent, config: ModuleConfig) -> anyhow::Result<()> {
    use MembershipStatus::*;

    let sender = event.sender.clone();
    let status = membership_status(sender.clone()).await?;
    let member = sender.localpart();

    let response = match membership_status(event.sender).await? {
        NotAMember => "not a member".s(),
        Stoned => "stoned".s(),
        Inactive => "not currently a member".s(),
        Active(months) => match months {
            i64::MIN..0 => format!("{} months ahead. Cool!", 0 - months),
            0 => format!("paid all membership fees."),
            1 => format!("need to pay one membership fee."),
            2..=i64::MAX => format!("need to pay {months} membership fees."),
        },
    };

    event
        .room
        .send(RoomMessageEventContent::text_plain(response))
        .await?;

    Ok(())
}

pub(crate) fn passthrough(
    mx: &Client,
    config: &Config,
) -> anyhow::Result<Vec<PassThroughModuleInfo>> {
    info!("registering passthrough modules");
    let mut modules: Vec<PassThroughModuleInfo> = vec![];
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;

    let (nagtx, nagrx) = mpsc::channel::<ConsumerEvent>(1);
    tokio::task::spawn(nag_consumer(nagrx, module_config.clone()));
    let nag = PassThroughModuleInfo(ModuleInfo {
        name: "nag".s(),
        help: "nags users about missing membership fees".s(),
        acl: vec![Acl::Room(module_config.nag_channels)],
        trigger: TriggerType::Catchall(|_, _, _, _, _| Ok(Consumption::Inclusive)),
        channel: Some(nagtx),
    });
    modules.push(nag);

    Ok(modules)
}

/* with no persistent store access (no async), this would've been effectively just a room acl
fn nag_decider(
    klaczlevel: i64,
    sender: OwnedUserId,
    room: &Room,
    content: &RoomMessageEventContent,
    config: &Config,
) -> anyhow::Result<Consumption> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;

    if module_config.nag_channels.contains(&room_name(room)) {
        return Ok(Consumption::Reject);
    };

    Ok(Consumption::Inclusive)
}
*/

async fn nag_consumer(
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

        if let Err(e) = nag(event.clone(), config.clone()).await {
            error!("error while processing nag check: {e}");
        }
    }
}

async fn nag(event: ConsumerEvent, config: ModuleConfig) -> anyhow::Result<()> {
    use MembershipStatus::*;
    let sender_str: &str = event.sender.as_str();

    trace!("getting client store");
    let c = event.room.client();
    let store = c.state_store();
    let next_nag_time = match store.get_custom_value(event.sender.as_bytes()).await {
        Ok(maybe_result) => match maybe_result {
            None => {
                let nag_time = NotBotTime(SystemTime::now() - Duration::new(60 * 60 * 24, 0));
                let nag_time_bytes: Vec<u8> = nag_time.into();

                store
                    .set_custom_value_no_read(event.sender.as_bytes(), nag_time_bytes)
                    .await?;

                nag_time
            }
            Some(nag_time_bytes) => nag_time_bytes.into(),
        },
        Err(e) => bail!("error fetching nag time: {e}"),
    };

    trace!("next_nag_time: {:#?}", next_nag_time);

    if NotBotTime::now() > next_nag_time {
        let next_nag_time = NotBotTime(SystemTime::now() + Duration::new(60 * 60 * 24, 0));

        store
            .set_custom_value_no_read(event.sender.as_bytes(), next_nag_time.into())
            .await?;
    } else {
        return Ok(());
    };

    let Ok(Active(months)) = membership_status(event.sender.clone()).await else {
        return Ok(());
    };

    if months < config.nag_late_fees {
        debug!("too early to nag: {months}");
        return Ok(());
    };

    let period = match months {
        i64::MIN..=0 => {
            return Ok(());
        }
        1 => "month",
        _ => "months",
    };

    let member_display_name: String = match event.room.get_member(&event.sender).await {
        Ok(Some(rm)) => match rm.display_name() {
            Some(d) => d.to_owned(),
            None => sender_str.to_owned(),
        },
        _ => sender_str.to_owned(),
    };

    let msg_text = format!("pay your membership fees! you are {months} {period} behind!");
    let plain_message = format!(
        r#"{display_name}: {text}"#,
        display_name = member_display_name,
        text = msg_text
    );

    let html_message = format!(
        r#"<a href="{uri}">{display_name}</a>: {text}"#,
        uri = event.sender.matrix_to_uri(),
        display_name = member_display_name,
        text = msg_text
    );

    let msg = RoomMessageEventContent::text_html(plain_message, html_message)
        .add_mentions(Mentions::with_user_ids(vec![event.sender]));

    event.room.send(msg).await?;

    Ok(())
}
