//! Interact with Warsaw Hackerspace membership fees tracking system
//!
//! # Configuration
//!
//! ```toml
//! [module."notbot::kasownik"]
//! # List of strings; required; rooms on which bot will nag active members about late membership fees.
//! nag_channels = [
//!     "#bottest:example.com",
//!     "#members:example.org",
//!     "#notbot-test-private-room:example.com"
//! ]
//! # Late fees leniency in months
//! nag_late_fees = 0
//! # List of strings; required; channels on which known members are allowed to check fees status of other members.
//! due_others_allowed = [
//!     "#bottest:example.com",
//!     "#members:example.org",
//!     "#notbot-test-private-room:example.com"
//! ]
//! ```
//!
//! # Usage
//!
//! ```chat logs
//! <foo> ~due bar
//! <notbot> bar is 5 months ahead. Cool!
//! <baz> ~due-me
//! <notbot> baz needs to pay 8 membership fees.
//! <xyz123> hi
//! <notbot> @xyz123:example.org: pay your membership fees! you are 2 months behind!
//! ```

use crate::prelude::*;

use crate::notbottime::NotBotTime;

fn default_due_keywords() -> Vec<String> {
    vec!["due".s()]
}

fn default_due_me_keywords() -> Vec<String> {
    vec!["due-me".s(), "dueme".s()]
}

#[derive(Clone, Deserialize)]
struct ModuleConfig {
    nag_channels: Vec<String>,
    nag_late_fees: i64,
    due_others_allowed: Vec<String>,
    #[serde(default = "default_due_keywords")]
    keywords_due: Vec<String>,
    #[serde(default = "default_due_me_keywords")]
    keywords_due_me: Vec<String>,
}

pub(crate) fn starter(_: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering modules");
    let module_config: ModuleConfig = config.typed_module_config(module_path!())?;

    let (duetx, duerx) = mpsc::channel::<ConsumerEvent>(1);
    let due = ModuleInfo {
        name: "due".s(),
        help: "checks how many membership fees a member is missing".s(),
        acl: vec![Acl::Room(module_config.due_others_allowed.clone())],
        trigger: TriggerType::Keyword(module_config.keywords_due.clone()),
        channel: duetx,
        error_prefix: Some("error checking membership fees".s()),
    };
    due.spawn(duerx, module_config.clone(), due_processor);

    let (due_metx, due_merx) = mpsc::channel::<ConsumerEvent>(1);
    let due_me = ModuleInfo {
        name: "due-me".s(),
        help: "checks how many membership fees you are missing".s(),
        acl: vec![],
        trigger: TriggerType::Keyword(module_config.keywords_due_me.clone()),
        channel: due_metx,
        error_prefix: Some("error checking membership fees".s()),
    };
    due_me.spawn(due_merx, module_config.clone(), due_me_processor);

    Ok(vec![due, due_me])
}

async fn due_processor(event: ConsumerEvent, _: ModuleConfig) -> anyhow::Result<()> {
    use MembershipStatus::*;

    if event.args.is_none() {
        event
            .room
            .send(RoomMessageEventContent::text_plain(
                "missing argument: member".s(),
            ))
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
                .send(RoomMessageEventContent::text_plain(
                    "member argument missing or couldn't parse".s(),
                ))
                .await?;
            return Ok(());
        };

        candidate.unwrap()
    };

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

async fn due_me_processor(event: ConsumerEvent, _: ModuleConfig) -> anyhow::Result<()> {
    use MembershipStatus::*;

    let response = match membership_status(event.sender).await? {
        NotAMember => "not a member".s(),
        Stoned => "stoned".s(),
        Inactive => "not currently a member".s(),
        Active(months) => match months {
            i64::MIN..0 => format!("{} months ahead. Cool!", 0 - months),
            0 => "paid all membership fees.".s(),
            1 => "need to pay one membership fee.".s(),
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
    _: &Client,
    config: &Config,
) -> anyhow::Result<Vec<PassThroughModuleInfo>> {
    info!("registering passthrough modules");
    let module_config: ModuleConfig = config.typed_module_config(module_path!())?;

    let (nagtx, nagrx) = mpsc::channel::<ConsumerEvent>(1);
    let nag = PassThroughModuleInfo(ModuleInfo {
        name: "nag".s(),
        help: "nags users about missing membership fees".s(),
        acl: vec![Acl::Room(module_config.nag_channels.clone())],
        trigger: TriggerType::Catchall(|_, _, _, _, _| Ok(Consumption::Inclusive)),
        channel: nagtx,
        error_prefix: None,
    });
    nag.0.spawn(nagrx, module_config, nag_processor);

    Ok(vec![nag])
}

async fn nag_processor(event: ConsumerEvent, config: ModuleConfig) -> anyhow::Result<()> {
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
