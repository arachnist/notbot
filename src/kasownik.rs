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

use crate::{prelude::*, tools};

use tokio_postgres::types::Type as dbtype;

fn default_due_keywords() -> Vec<String> {
    vec!["due".s()]
}

fn default_due_me_keywords() -> Vec<String> {
    vec!["due-me".s(), "dueme".s()]
}

fn members_only_rooms() -> Vec<String> {
    vec![]
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
    /// Database handle for nag persistence
    pub handle: String,
    /// List of members-only matrix rooms/spaces.
    #[serde(default = "members_only_rooms")]
    pub members_only_rooms: Vec<String>,
}

pub(crate) fn starter(_: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering modules");
    let module_config: ModuleConfig = config.typed_module_config(module_path!())?;

    Ok(vec![
        ModuleInfo::new(
            "due",
            "checks how many membership fees a member is missing",
            vec![Acl::Room(module_config.due_others_allowed.clone())],
            TriggerType::Keyword(module_config.keywords_due.clone()),
            Some("error checking membership fees"),
            module_config.clone(),
            due_processor,
        ),
        ModuleInfo::new(
            "due-me",
            "checks how many membership fees you are missing",
            vec![],
            TriggerType::Keyword(module_config.keywords_due_me.clone()),
            Some("error checking membership fees"),
            module_config.clone(),
            due_me_processor,
        ),
        ModuleInfo::new(
            "debtors",
            "lists users present on members-only rooms that have their hswaw membership marked as Inactive",
            vec![Acl::Room(vec![
                "#bottest:is-a.cat".s(),
                "#notbot-test-private-room:is-a.cat".s(),
            ])],
            TriggerType::Keyword(vec!["debtors".s()]),
            Some("error getting debtors list"),
            module_config,
            list_late,
        ),
    ])
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

async fn list_late(event: ConsumerEvent, c: ModuleConfig) -> anyhow::Result<()> {
    use MembershipStatus::{Inactive, NotAMember};

    let mut debtors: HashMap<String, Vec<String>> = HashMap::default();
    let mut not_members: HashMap<String, Vec<String>> = HashMap::default();

    for room_name in c.members_only_rooms {
        trace!("checking room: {room_name}");
        let room = tools::maybe_get_room(&event.room.client(), &room_name).await?;

        // `ACTIVE` here means users that are joined or invited
        for room_member in room.members(matrix_sdk::RoomMemberships::ACTIVE).await? {
            let mxid: String = room_member.user_id().to_string();
            trace!("checking member: {}", mxid);
            match tools::membership_status(
                c.capacifier_token.clone(),
                room_member.user_id().to_owned(),
            )
            .await
            {
                Ok(m) => match m {
                    Inactive => debtors
                        .entry(room_name.clone())
                        .or_insert(vec![])
                        .push(mxid),
                    NotAMember => not_members
                        .entry(room_name.clone())
                        .or_insert(vec![])
                        .push(mxid),
                    _ => continue,
                },
                Err(e) => {
                    error!("error checking membership status: {e}");
                    continue;
                }
            };
        }
    }

    let mut response_parts: Vec<String> = vec![];

    if !debtors.is_empty() {
        response_parts.push("debtors:\n".s());
    }

    for (room, mxids) in debtors.iter() {
        response_parts.push(format!("{}:\n{}\n", room, mxids.join(", ")));
    }

    if !not_members.is_empty() {
        response_parts.push("mxid not matched with a member:\n".s());
    }

    for (room, mxids) in not_members.iter() {
        response_parts.push(format!("{}:\n{}\n", room, mxids.join(", ")));
    }

    let response = response_parts.join(" ");

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

    Ok(vec![PassThroughModuleInfo(ModuleInfo::new(
        "nag",
        "nags users about missing membership fees",
        vec![Acl::Room(module_config.nag_channels.clone())],
        TriggerType::Catchall(|_, _, _, _, _| Ok(Consumption::Inclusive)),
        None,
        module_config,
        nag_processor,
    ))])
}

/// Nags members active in the chat about late membership fees, at most once every 24 hours.
///
/// # Errors
/// Will return error if sending nagging notification fails
pub async fn nag_processor(event: ConsumerEvent, config: ModuleConfig) -> anyhow::Result<()> {
    trace!("in nag_processor");
    use MembershipStatus::Active;
    let sender_str: &str = event.sender.as_str();
    trace!("getting member");
    let maybe_member: Option<Vec<String>> = capacifier_kvl_query(
        config.capacifier_token.clone(),
        "kvl",
        "uid",
        "matrixUserID",
        event.sender.to_string(),
    )
    .await?;

    trace!("extracting uid");
    let member_uid = match maybe_member {
        None => bail!("not a member"),
        Some(m) => match m.iter().next() {
            None => bail!("not a member"),
            Some(u) => u.to_owned(),
        },
    };

    trace!("building persistence object");
    let persistence = NagPersistence {
        handle: config.handle,
    };
    let next_nag_time = persistence.next_nag_time(&member_uid).await?;

    trace!("next_nag_time: {:#?}", next_nag_time);

    if SystemTime::now() < next_nag_time {
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

#[derive(Clone, Debug)]
struct NagPersistence {
    handle: String,
}

impl NagPersistence {
    const GET_NAG_TIME: &str = r"SELECT nag_time FROM kasownik_nag WHERE member = $1";
    const SET_NAG_TIME: &str = r"INSERT INTO kasownik_nag (member, nag_time)
    VALUES ( $1, $2 )
    ON CONFLICT (member) DO UPDATE
        SET nag_time = $2";
    async fn next_nag_time(&self, member: &str) -> anyhow::Result<SystemTime> {
        let now = SystemTime::now();
        let future_time = now
            .checked_add(Duration::from_secs(24 * 3600))
            .map_or(now, |e| e);

        let mut client = DBPools::get_client(&self.handle).await?;
        let transaction = client.transaction().await?;

        let get_nag = transaction
            .prepare_typed_cached(Self::GET_NAG_TIME, &[dbtype::VARCHAR])
            .await?;
        let set_nag = transaction
            .prepare_typed_cached(Self::SET_NAG_TIME, &[dbtype::VARCHAR, dbtype::TIMESTAMP])
            .await?;

        let nag_q = transaction.query(&get_nag, &[&member]).await?;
        let nag = match nag_q.len() {
            0 => {
                transaction
                    .execute(&set_nag, &[&member, &future_time])
                    .await?;

                SystemTime::UNIX_EPOCH
            }
            1 => {
                let t = match nag_q.first() {
                    None => bail!("wtf? db inconsistency: can't fetch first result row"),
                    Some(r) => r.try_get(0)?,
                };

                if t < now {
                    transaction
                        .execute(&set_nag, &[&member, &future_time])
                        .await?;
                };

                t
            }
            _ => bail!("wtf? db inconsistency: multiple entries per member in kasownik_nag"),
        };

        transaction.commit().await?;
        Ok(nag)
    }
}
