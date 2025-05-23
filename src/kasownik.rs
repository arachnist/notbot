//! Interact with Warsaw Hackerspace membership fees tracking system
//!
//! # Configuration
//!
//! [`ModuleConfig`]
//!
//! ```toml
//! [module."notbot::kasownik"]
//! nag_channels = [
//!     "#bottest:example.com",
//!     "#members:example.org",
//!     "#notbot-test-private-room:example.com"
//! ]
//! nag_late_fees = 0
//! due_others_allowed = [
//!     "#bottest:example.com",
//!     "#members:example.org",
//!     "#notbot-test-private-room:example.com"
//! ]
//! ```
//!
//! # Usage
//!
//! Keywords:
//! * `due <member>` - [`due_processor`] - check membership fees for others
//! * `due-me` - [`due_me_processor`] - check membership fees status for yourself
//!
//! Catch-all:
//! * [`nag_processor`] - events not consumed by other modules will trigger a check for fees status, and nag the user if they're late.

use crate::prelude::*;

use crate::notbottime::NotBotTime;

fn default_due_keywords() -> Vec<String> {
    vec!["due".s()]
}

fn default_due_me_keywords() -> Vec<String> {
    vec!["due-me".s(), "dueme".s()]
}

/// Module configuration object.
#[derive(Clone, Deserialize)]
pub struct ModuleConfig {
    /// Rooms on which bot will nag active members about late membership fees.
    pub nag_channels: Vec<String>,
    /// Late fees leniency in months.
    pub nag_late_fees: i64,
    /// Rooms on which users are allowed to check fees status of other members.
    pub due_others_allowed: Vec<String>,
    /// Keywords the [`due_processor`] will respond to
    #[serde(default = "default_due_keywords")]
    pub keywords_due: Vec<String>,
    /// Keywords the [`due_me_processor`] will respond to
    #[serde(default = "default_due_me_keywords")]
    pub keywords_due_me: Vec<String>,
    /// Token for querying capacifier
    pub capacifier_token: String,
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
    due_me.spawn(due_merx, module_config, due_me_processor);

    Ok(vec![due, due_me])
}

/// Processes checks for other user membership fees status.
///
/// If a message explicitly mentions someone, see [`matrix_sdk::ruma::events::Mentions`],
/// try using the first mentioned user. Otherwise, make a best-effort attempt
/// at parsing provided plaintext argument.
///
/// # Errors
/// Will return `Err` if:
/// * can't parse `due` target.
/// * checking membership status.
/// * sending response fails.
pub async fn due_processor(event: ConsumerEvent, c: ModuleConfig) -> anyhow::Result<()> {
    use MembershipStatus::{Active, Inactive, NotAMember, Stoned};

    let Some(arguments) = event.args else {
        bail!("missing argument: member");
    };

    let target = {
        let mut candidate: Option<OwnedUserId> = None;
        if let Some(mut mentions_set) = event.ev.content.mentions {
            trace!("mentions: {mentions_set:#?}");
            if let Some(userid) = mentions_set.user_ids.pop_first() {
                candidate = Some(userid);
            }
        }

        if candidate.is_none() {
            let mut args = arguments.split_whitespace();
            if let Some(plain_target) = args.next() {
                let maybe_mxid = format!("@{plain_target}:hackerspace.pl");
                candidate = UserId::parse(maybe_mxid).ok();
            };
        };

        let Some(found) = candidate else {
            bail!("member argument missing or we couldn't parse it");
        };

        found
    };

    let member = target.localpart();
    let response = match membership_status(c.capacifier_token, target.clone()).await? {
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

/// Processes checks for membership status of the user sending the event.
///
/// # Errors
/// Will return error if checking membership status, or sending response fails.
pub async fn due_me_processor(event: ConsumerEvent, c: ModuleConfig) -> anyhow::Result<()> {
    use MembershipStatus::{Active, Inactive, NotAMember, Stoned};

    let response = match membership_status(c.capacifier_token, event.sender).await? {
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

/// Nags members active in the chat about late membership fees, at most once every 24 hours.
///
/// # Errors
/// Will return error if sending nagging notification fails
pub async fn nag_processor(event: ConsumerEvent, config: ModuleConfig) -> anyhow::Result<()> {
    use MembershipStatus::Active;
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

    let Ok(Active(months)) = membership_status(config.capacifier_token, event.sender.clone()).await
    else {
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
        Ok(Some(rm)) => rm
            .display_name()
            .map_or_else(|| sender_str.to_owned(), std::borrow::ToOwned::to_owned),
        _ => sender_str.to_owned(),
    };

    let msg_text = format!("pay your membership fees! you are {months} {period} behind!");
    let plain_message = format!(r"{member_display_name}: {msg_text}");

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
