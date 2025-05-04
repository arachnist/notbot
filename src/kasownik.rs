use crate::prelude::*;

use crate::notbottime::NotBotTime;

use serde_json::Value;

use leon::{vals, Template};

use prometheus::Counter;
use prometheus::{opts, register_counter};

lazy_static! {
    static ref DUE_CHECKS: Counter =
        register_counter!(opts!("due_checks_total", "Number of due checks",)).unwrap();
    static ref DUE_CHECKS_SUCCESSFUL: Counter = register_counter!(opts!(
        "due_checks_successful",
        "Number of successful due checks",
    ))
    .unwrap();
    static ref NAGS: Counter =
        register_counter!(opts!("nags_total", "Number of times members were nagged",)).unwrap();
}

pub(crate) fn modules() -> Vec<ModuleStarter> {
    vec![
        (module_path!(), module_starter),
        ("notbot::kasownik::nag", module_starter_nag),
    ]
}

fn module_starter_nag(client: &Client, config: &Config) -> anyhow::Result<EventHandlerHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
    Ok(client
        .add_event_handler(move |ev, room, c| module_entrypoint_nag(ev, room, c, module_config)))
}

#[derive(Clone, Deserialize)]
pub struct ModuleConfig {
    pub url_template: String,
    pub nag_channels: Vec<String>,
    pub nag_late_fees: i64,
}

fn module_starter(client: &Client, config: &Config) -> anyhow::Result<EventHandlerHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
    Ok(client.add_event_handler(move |ev, room| {
        simple_command_wrapper(
            ev,
            room,
            module_config,
            vec!["~due", ".due", "~due-me", ".due-me"]
                .into_iter()
                .map(|x| x.to_string())
                .collect(),
            due,
        )
    }))
}

async fn due(
    _room: Room,
    sender: OwnedUserId,
    keyword: String,
    argv: Vec<String>,
    config: ModuleConfig,
) -> anyhow::Result<String> {
    let member: String = match keyword.as_str() {
        ".due" | "~due" => {
            let Some(m) = argv.first() else {
                bail!("missing argument: member")
            };

            m.to_owned()
        }
        ".due-me" | "~due-me" => sender.localpart().trim_start_matches("libera_").to_string(),
        _ => {
            bail!("wtf? unmatched keyword passed somehow. we shouldn't be here",)
        }
    };

    DUE_CHECKS.inc();

    let url_template = Template::parse(&config.url_template)?;

    let url = url_template.render(&&vals(|key| {
        if key == "member" {
            Some(member.clone().into())
        } else {
            None
        }
    }))?;

    let client = RClient::new();
    let response = client.get(url).send().await?;

    match response.status().as_u16() {
        404 => Ok("No such member.".to_string()),
        410 => Ok("HTTP 410 Gone.".to_string()),
        420 => Ok("HTTP 420 Stoned.".to_string()),
        200 => {
            let data = response.json::<Kasownik>().await?;

            if data.status != "ok" {
                bail!("No such member.");
            };

            let response = match data.content.as_i64() {
                None => {
                    bail!("content returned from kasownik doesn't parse as integer: {data:#?}")
                }
                Some(months) => match months {
                    i64::MIN..0 => format!("{member} is {} months ahead. Cool!", 0 - months),
                    0 => format!("{member} has paid all their membership fees."),
                    1 => format!("{member} needs to pay one membership fee."),
                    2..=i64::MAX => format!("{member} needs to pay {months} membership fees."),
                },
            };

            DUE_CHECKS_SUCCESSFUL.inc();

            Ok(response)
        }
        _ => bail!("kasownik responded with weird status code",),
    }
}

async fn module_entrypoint_nag(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    c: Client,
    module_config: ModuleConfig,
) -> anyhow::Result<()> {
    if let Some(alias) = room.canonical_alias() {
        if !module_config
            .nag_channels
            .iter()
            .any(|x| x == alias.as_str())
        {
            return Ok(());
        };
    } else {
        return Ok(());
    };

    let sender_str: &str = ev.sender.as_str();
    let member: String = ev.sender.localpart().to_owned();

    trace!("getting client store");
    let store = c.state_store();
    let next_nag_time = match store.get_custom_value(ev.sender.as_bytes()).await {
        Ok(maybe_result) => match maybe_result {
            None => {
                let nag_time = NotBotTime(SystemTime::now() - Duration::new(60 * 60 * 24, 0));
                let nag_time_bytes: Vec<u8> = nag_time.into();

                store
                    .set_custom_value_no_read(ev.sender.as_bytes(), nag_time_bytes)
                    .await?;

                nag_time
            }
            Some(nag_time_bytes) => nag_time_bytes.into(),
        },
        Err(e) => bail!("error fetching nag time: {e}"),
    };

    trace!("next_nag_time: {:#?}", next_nag_time);

    if NotBotTime::now() > next_nag_time {
        debug!("member not checked recently: {sender_str}");
        let next_nag_time = NotBotTime(SystemTime::now() + Duration::new(60 * 60 * 24, 0));

        store
            .set_custom_value_no_read(ev.sender.as_bytes(), next_nag_time.into())
            .await?;
    } else {
        debug!("member checked recently, ignoring: {sender_str}");
        return Ok(());
    };

    let url_template = Template::parse(&module_config.url_template)?;

    let url = url_template.render(&&vals(|key| {
        if key == "member" {
            Some(member.clone().into())
        } else {
            None
        }
    }))?;

    let data: Kasownik = fetch_and_decode_json(url).await?;
    trace!("returned data: {:#?}", data);

    let Some(months) = data.content.as_i64() else {
        bail!("kasownik returned weird data: {data:#?}")
    };

    if months < module_config.nag_late_fees {
        debug!("too early to nag: {months}");
        return Ok(());
    };

    NAGS.inc();

    let period = match months {
        i64::MIN..=0 => {
            return Ok(());
        }
        1 => "month",
        _ => "months",
    };

    let member_display_name: String = match room.get_member(&ev.sender).await {
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
        uri = ev.sender.matrix_to_uri(),
        display_name = member_display_name,
        text = msg_text
    );

    let msg = RoomMessageEventContent::text_html(plain_message, html_message)
        .add_mentions(Mentions::with_user_ids(vec![ev.sender]));

    room.send(msg).await?;

    Ok(())
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct Kasownik {
    pub status: String,
    pub content: Value,
    pub modified: String,
}
