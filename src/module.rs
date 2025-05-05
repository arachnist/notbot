use std::ops::Deref;

use crate::config::Config;
use crate::klaczdb::KlaczDB;
use crate::tools::{membership_status, room_name, ToStringExt};

use anyhow::bail;
use lazy_static::lazy_static;
use prometheus::{opts, register_int_counter_vec, IntCounterVec};
use tracing::{debug, error, info, trace};

use matrix_sdk::event_handler::{Ctx, EventHandlerHandle};
use matrix_sdk::ruma::events::room::message::{
    MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
};
use matrix_sdk::ruma::OwnedUserId;
use matrix_sdk::{Client, Room};

use tokio::sync::mpsc;

use mlua::Lua;

lazy_static! {
    static ref MODULE_EVENTS: IntCounterVec = register_int_counter_vec!(
        opts!(
            "module_event_counts",
            "Number of events a module has consumed"
        ),
        &["event"]
    )
    .unwrap();
}

#[derive(Clone)]
pub struct ConsumerEvent {
    pub klacz_level: i64, // klacz level
    pub ev: OriginalSyncRoomMessageEvent,
    pub sender: OwnedUserId,
    pub room: Room,
    pub keyword: String,
    pub args: Option<String>,
    pub lua: Lua,
    pub klacz: KlaczDB,
}

#[derive(Clone, Debug)]
pub struct ModuleInfo {
    pub name: String,
    pub help: String,
    pub acl: Vec<Acl>,
    pub trigger: TriggerType,
    pub channel: Option<mpsc::Sender<ConsumerEvent>>,
}

// distinct from generic ModuleInfo for legal purposes only
#[derive(Clone)]
pub struct PassThroughModuleInfo(pub ModuleInfo);

// figure out how to pass just limited config here
pub type CatchallDecider = fn(
    klaczlevel: i64,
    sender: OwnedUserId,
    room: &Room,
    content: &RoomMessageEventContent,
    config: &Config,
) -> anyhow::Result<Consumption>;

#[derive(Clone, Debug)]
pub enum TriggerType {
    Keyword(Vec<String>),
    Catchall(CatchallDecider),
}

#[derive(Clone, Debug)]
pub enum Acl {
    ActiveHswawMember,
    MaybeInactiveHswawMember,
    SpecificUsers(Vec<String>),
    Room(Vec<String>),
    KlaczLevel(i64),
    Homeserver(Vec<String>),
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug)]
pub enum Consumption {
    // module doesn't want this event
    Reject,
    // module wants this event, but in a passive way; shouldn't be later rejected with ACLs (noisy)
    Inclusive,
    // module wants this event exclusively, but doesn't mind if passthrough modules catch it as well
    Passthrough,
    // module wants this event exclusively. mostly for keyworded commands
    Exclusive,
}

#[allow(clippy::too_many_arguments)]
pub async fn dispatcher(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    config: Ctx<Config>,
    modules: Ctx<Vec<ModuleInfo>>,
    passthrough_modules: Ctx<Vec<PassThroughModuleInfo>>,
    klacz: Ctx<KlaczDB>,
    lua: Ctx<Lua>,
) -> anyhow::Result<()> {
    use Consumption::*;

    let sender: OwnedUserId = ev.sender.clone();

    /* we tried to be fancy here, but formatted events are fancier
    let (body, formatted) = match ev.content.msgtype {
        MessageType::Text(ref content) => (content.body.clone(), content.formatted.clone()),
        MessageType::Notice(ref content) => (content.body.clone(), content.formatted.clone()),
        _ => return Ok(()),
    };

    let text = match formatted {
        Some(content) => match content.format {
            MessageFormat::Html => content.body,
            _ => body,
        },
        None => body,
    };
    */

    let MessageType::Text(ref content) = ev.content.msgtype else {
        return Ok(());
    };
    let text = content.body.clone();

    trace!("new dispatcher: getting klacz permission level");
    let klacz_level = match klacz.get_level(&room, &sender).await {
        Ok(level) => level,
        Err(e) => {
            error!("error getting klacz permission level: {e}");
            0
        }
    };

    let mut args = text.trim_start().splitn(2, [' ', ' ', '\t']);
    let first = args.next();

    trace!("cursed prefix matching");
    let (keyword, remainder): (String, Option<String>) = {
        match first {
            None => (String::new(), None),
            Some(word) => {
                trace!("first word exists: {word}");
                let (mut kw_candidate, mut remainder_candidate) = (String::new(), None);
                for prefix in config.prefixes() {
                    trace!("trying prefix: {prefix}");
                    match prefix.len() {
                        1 => match word.strip_prefix(prefix.as_str()) {
                            None => continue,
                            Some(w) => {
                                kw_candidate = w.to_string();
                                remainder_candidate = args.next().map(|s| s.to_string());
                                trace!("selected prefix: {prefix}");
                                break;
                            }
                        },
                        // meme command case
                        2.. => {
                            if word == prefix {
                                if let Some(shifted_text) = args.next() {
                                    let mut shifted_args =
                                        shifted_text.trim_start().splitn(2, [' ', ' ', '\t']);
                                    if let Some(second) = shifted_args.next() {
                                        kw_candidate = second.to_string();
                                        remainder_candidate =
                                            shifted_args.next().map(|s| s.to_string());
                                    };
                                };
                                trace!("selected prefix: {prefix}");
                                break;
                            };
                        }
                        0 => continue,
                    };
                }

                (kw_candidate, remainder_candidate)
            }
        }
    };

    let consumer_event = ConsumerEvent {
        klacz_level,
        ev: ev.clone(),
        sender: sender.clone(),
        room: room.clone(),
        keyword: keyword.clone(),
        args: remainder,
        lua: lua.deref().clone(),
        klacz: klacz.deref().clone(),
    };
    // trace!("consumer event: {consumer_event:#?}");

    let mut run_modules: Vec<(Consumption, ModuleInfo)> = vec![];
    let mut consumption = Inclusive;

    // go through all the modules first to figure out consumption priority
    trace!("figuring out event consumption priority");
    for module in modules.iter() {
        trace!("considering module: {}", module.name);
        use TriggerType::*;

        if module.channel.is_none() {
            debug!("failed module, skipping");
            continue;
        };

        let module_consumption: Consumption = match module.trigger {
            Keyword(ref keywords) => {
                if !keyword.is_empty() && keywords.contains(&keyword) {
                    Exclusive
                } else {
                    trace!("\"{keyword}\" doesn't match any keyword: {keywords:?}");
                    Reject
                }
            }
            Catchall(fun) => match fun(
                klacz_level,
                sender.clone(),
                &room,
                &ev.content,
                &config.clone(),
            ) {
                Err(e) => {
                    error!("{} decider returned error: {e}", module.name);
                    continue;
                }
                Ok(Reject) => continue,
                Ok(c) => c,
            },
        };

        trace!("checking consumption: {module_consumption:?}");
        // while technically there might be a situation where multiple modules would have a chance to return with
        // exclusive consumption, i don't think there's a better solution than running just the first one found
        match module_consumption {
            Exclusive => {
                run_modules.truncate(0);
                run_modules.push((module_consumption.clone(), module.clone()));
                consumption = module_consumption.clone();
                break;
            }
            Passthrough => {
                run_modules.retain(|x| x.0 == Passthrough);
                run_modules.push((module_consumption.clone(), module.clone()));
                consumption = module_consumption;
            }
            Inclusive => {
                if consumption > module_consumption {
                    continue;
                } else {
                    run_modules.push((module_consumption, module.clone()));
                }
            }
            Reject => continue,
        };
    }

    trace!("dispatching event to modules");
    for (_, module) in run_modules {
        if let Err(e) = dispatch_module(
            true,
            &module,
            klacz_level,
            sender.clone(),
            room.clone(),
            consumer_event.clone(),
        )
        .await
        {
            error!("dispatching module {} failed: {e}", module.name);
        };
    }

    match consumption {
        Consumption::Inclusive | Consumption::Passthrough => {
            trace!("dispatching event to passthrough modules");
            let mut run_passthrough_modules: Vec<ModuleInfo> = vec![];

            use TriggerType::*;

            for module in passthrough_modules.iter() {
                // we're in passthrough already, so we don't
                match module.0.trigger {
                    Keyword(_) => {
                        // handle that on init?
                        error!("can't have keyword modules in passthrough!");
                        continue;
                    }
                    Catchall(fun) => match fun(
                        klacz_level,
                        sender.clone(),
                        &room,
                        &ev.content,
                        &config.clone(),
                    ) {
                        Err(e) => {
                            error!("{} decider returned error: {e}", module.0.name);
                            continue;
                        }
                        Ok(Reject) => continue,
                        Ok(_) => run_passthrough_modules.push(module.0.clone()),
                    },
                };
            }

            for module in run_passthrough_modules {
                if module.channel.is_none() {
                    debug!("failed module, skipping");
                    continue;
                };

                if let Err(e) = dispatch_module(
                    false,
                    &module,
                    klacz_level,
                    sender.clone(),
                    room.clone(),
                    consumer_event.clone(),
                )
                .await
                {
                    error!("dispatching passthrough module {} failed: {e}", module.name);
                };
            }
        }
        _ => {
            debug!("skipping passthrough modules");
        }
    }

    Ok(())
}

async fn dispatch_module(
    general: bool,
    module: &ModuleInfo,
    klacz_level: i64,
    sender: OwnedUserId,
    room: Room,
    consumer_event: ConsumerEvent,
) -> anyhow::Result<()> {
    trace!("dispatching module: {}", module.name);
    use Acl::*;

    let mut failed = false;

    for acl in &module.acl {
        trace!("checking acl: {acl:#?}");
        match acl {
            KlaczLevel(required) => {
                trace!("required: {required}, current: {klacz_level}");
                if required > &klacz_level {
                    failed = true;
                }
            }
            Homeserver(homeservers) => {
                if !homeservers.contains(&sender.clone().server_name().to_string()) {
                    failed = true;
                }
            }
            Room(rooms) => {
                let name = room_name(&room);
                if !rooms.contains(&name) {
                    failed = true;
                }
            }
            SpecificUsers(users) => {
                if !users.contains(&sender.to_string()) {
                    failed = true;
                }
            }
            ActiveHswawMember => {
                use crate::tools::MembershipStatus::*;
                match membership_status(sender.clone()).await {
                    Err(e) => {
                        error!("checking membership for {sender} failed: {e}");
                        failed = true;
                    }
                    Ok(status) => match status {
                        Inactive | Stoned | NotAMember => failed = true,
                        // kasownik, and - by extension - the board, has authority on who is an active member
                        Active(_) => (),
                    },
                }
            }
            MaybeInactiveHswawMember => {
                use crate::tools::MembershipStatus::*;
                match membership_status(sender.clone()).await {
                    Err(e) => {
                        error!("checking membership for {sender} failed: {e}");
                        failed = true;
                    }
                    Ok(status) => match status {
                        Stoned | NotAMember => failed = true,
                        Inactive | Active(_) => (),
                    },
                }
            }
        };

        if failed {
            break;
        }
    }

    if failed {
        if general {
            room.send(RoomMessageEventContent::text_plain(
                "I'm sorry Dave, I'm afraid I can't do that",
            ))
            .await?;
        };
        return Ok(());
    };

    trace!("attempting to reserve channel space");
    // .unwrap() is safe here because star
    let reservation = match module.channel.clone().unwrap().try_reserve_owned() {
        Ok(r) => r,
        Err(e) => {
            error!("module {} channel can't accept message: {e}", module.name);
            return Ok(());
        }
    };

    MODULE_EVENTS.with_label_values(&[&module.name]).inc();
    trace!("sending event");
    reservation.send(consumer_event);

    Ok(())
}

pub fn init_modules(mx: &Client, config: &Config) -> anyhow::Result<EventHandlerHandle> {
    let klacz = KlaczDB { handle: "main" };
    let mut modules: Vec<ModuleInfo> = vec![];
    let mut passthrough_modules: Vec<PassThroughModuleInfo> = vec![];

    // start moving notmun to becoming a 1st-class citizen
    match crate::notmun::module_starter(mx, config) {
        Err(e) => error!("failed initializing notmun: {e}"),
        Ok(()) => info!("initialized notmun"),
    };

    for starter in [
        crate::klaczdb::starter,
        crate::spaceapi::starter,
        crate::db::starter,
        crate::inviter::starter,
        crate::kasownik::starter,
        crate::wolfram::starter,
        crate::sage::starter,
    ] {
        match starter(mx, config) {
            Err(e) => error!("module initialization failed fatally: {e}"),
            Ok(m) => modules.extend(m),
        };
    }

    for starter in [crate::kasownik::passthrough, crate::notmun::starter] {
        match starter(mx, config) {
            Err(e) => error!("module initialization failed fatally: {e}"),
            Ok(m) => passthrough_modules.extend(m),
        };
    }

    match core_starter(modules.clone(), passthrough_modules.clone()) {
        Err(e) => error!("core modules module initialization failed fatally: {e}"),
        Ok(m) => modules.extend(m),
    };

    mx.add_event_handler_context(klacz);
    mx.add_event_handler_context(config.clone());
    mx.add_event_handler_context(modules);
    mx.add_event_handler_context(passthrough_modules);

    Ok(mx.add_event_handler(dispatcher))
}

pub(crate) fn core_starter(
    mut registered_modules: Vec<ModuleInfo>,
    registered_passthrough_modules: Vec<PassThroughModuleInfo>,
) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering modules");
    let mut modules: Vec<ModuleInfo> = vec![];

    let (help_tx, help_rx) = mpsc::channel::<ConsumerEvent>(1);
    let help = ModuleInfo {
        name: "help".s(),
        help: "get help about the bot or its basic functions".s(),
        acl: vec![],
        trigger: TriggerType::Keyword(vec!["help".s(), "status".s()]),
        channel: Some(help_tx),
    };
    modules.push(help);

    let (list_tx, list_rx) = mpsc::channel::<ConsumerEvent>(1);
    let list = ModuleInfo {
        name: "list".s(),
        help: "get the list of currently registered modules".s(),
        acl: vec![],
        trigger: TriggerType::Keyword(vec!["list".s(), "list-functions".s()]),
        channel: Some(list_tx),
    };
    modules.push(list);

    registered_modules.extend(modules.clone());
    tokio::task::spawn(help_consumer(
        help_rx,
        registered_modules.clone(),
        registered_passthrough_modules.clone(),
    ));
    tokio::task::spawn(list_consumer(
        list_rx,
        registered_modules,
        registered_passthrough_modules,
    ));

    Ok(modules)
}

async fn help_consumer(
    mut rx: mpsc::Receiver<ConsumerEvent>,
    registered_modules: Vec<ModuleInfo>,
    registered_passthrough_modules: Vec<PassThroughModuleInfo>,
) -> anyhow::Result<()> {
    loop {
        let event = match rx.recv().await {
            Some(e) => e,
            None => {
                error!("channel closed, goodbye! :(");
                bail!("channel closed");
            }
        };

        if let Err(e) = help_processor(
            event.clone(),
            registered_modules.clone(),
            registered_passthrough_modules.clone(),
        )
        .await
        {
            if let Err(e) = event
                .room
                .send(RoomMessageEventContent::text_plain(format!(
                    "error getting help: {e}"
                )))
                .await
            {
                error!("error while sending wolfram response: {e}");
            };
        }
    }
}

async fn help_processor(
    event: ConsumerEvent,
    registered_modules: Vec<ModuleInfo>,
    registered_passthrough_modules: Vec<PassThroughModuleInfo>,
) -> anyhow::Result<()> {
    let generic_help = {
        let modules_num = registered_modules.len();
        let passthrough_num = registered_passthrough_modules.len();
        let failed_num = registered_modules
            .iter()
            .filter(|x| x.channel.is_none())
            .count();
        let passthrough_failed_num = registered_passthrough_modules
            .iter()
            .filter(|x| x.0.channel.is_none())
            .count();
        let failed_mod_str = match failed_num {
            0 => String::new(),
            _ => format!(", of which {failed_num} did not initialize correctly"),
        };
        let failed_passthrough_str = match passthrough_failed_num {
            0 => String::new(),
            _ => format!(", of which {passthrough_failed_num} did not initialize correctly"),
        };
        let m_plural = if modules_num != 1 { "s" } else { "" };
        let p_plural = if passthrough_num != 1 { "s" } else { "" };
        let plain = format!(
            r#"this is the notbot module; source {source_url}
there are currently {modules_num} module{m_plural} registered{failed_mod_str}{maybe_newline} {passthrough_num} passthrough module{p_plural} registered{failed_passthrough_str}<br />
call «list-modules» to get a list of modules, or «help <module name>» for brief description of the module function
documentation is WIP
contact {mx_contact} or {fedi_contact} if you need more help"#,
            maybe_newline = if failed_num == 0 {
                ", and "
            } else {
                "\nthere are currently "
            },
            source_url = ": https://code.hackerspace.pl/ar/notbot",
            mx_contact = "matrix: @ar:is-a.cat",
            fedi_contact = "fedi: @ar@is-a.cat",
        );
        let html = format!(
            r#"this is the notbot module; source {source_url}<br />
there are currently {modules_num} module{m_plural} registered{failed_mod_str}{maybe_newline} {passthrough_num} passthrough module{p_plural} registered{failed_passthrough_str}<br />
call «list-modules» to get a list of modules, or «help <module name>» for brief description of the module function<br />
documentation is WIP<br />
contact {mx_contact} or {fedi_contact} if you need more help"#,
            maybe_newline = if failed_num == 0 {
                ", and "
            } else {
                "<br />\nthere are currently "
            },
            source_url = r#"is <a href="https://code.hackerspace.pl/ar/notbot">here</a>"#,
            mx_contact = r#"<a href="https://matrix.to/#/@ar:is-a.cat">@ar:is-a.cat</a>"#,
            fedi_contact = r#"<a href="https://is-a.cat/@ar">@ar@is-a.cat</a>"#,
        );

        RoomMessageEventContent::text_html(plain, html)
    };

    let Some(args) = event.args else {
        event.room.send(generic_help).await?;
        return Ok(());
    };

    let mut argv = args.split_whitespace();
    let Some(maybe_module_name) = argv.next() else {
        event.room.send(generic_help).await?;
        return Ok(());
    };

    let mut specific_response: Option<RoomMessageEventContent> = None;
    for module in registered_modules {
        if module.name == maybe_module_name {
            let response = format!(
                r#"module {name} is {status}; help: {help}"#,
                name = module.name,
                help = module.help,
                status = if module.channel.is_some() {
                    "running"
                } else {
                    "failed"
                },
            );
            specific_response = Some(RoomMessageEventContent::text_plain(response));
            break;
        };
    }

    for module in registered_passthrough_modules {
        if module.0.name == maybe_module_name {
            let keywords = match module.0.trigger {
                TriggerType::Keyword(t) => format!("; registered keywords: {t:?}"),
                _ => String::new(),
            };
            let response = format!(
                r#"module {name} is {status}; help: {help}{keywords}"#,
                name = module.0.name,
                help = module.0.help,
                status = if module.0.channel.is_some() {
                    "running"
                } else {
                    "failed"
                },
            );
            specific_response = Some(RoomMessageEventContent::text_plain(response));
            break;
        };
    }

    if let Some(response) = specific_response {
        event.room.send(response).await?;
    } else {
        event.room.send(generic_help).await?;
    };
    Ok(())
}

async fn list_consumer(
    mut rx: mpsc::Receiver<ConsumerEvent>,
    registered_modules: Vec<ModuleInfo>,
    registered_passthrough_modules: Vec<PassThroughModuleInfo>,
) -> anyhow::Result<()> {
    loop {
        let event = match rx.recv().await {
            Some(e) => e,
            None => {
                error!("channel closed, goodbye! :(");
                bail!("channel closed");
            }
        };

        let mut failed = false;
        let modules: Vec<String> = registered_modules
            .iter()
            .map(|x| {
                let mut s = x.name.clone();
                if x.channel.is_none() {
                    s.push('*');
                    failed = true;
                }
                s
            })
            .collect();
        let passthrough: Vec<String> = registered_passthrough_modules
            .iter()
            .map(|x| {
                let mut s = x.0.name.clone();
                if x.0.channel.is_none() {
                    s.push('*');
                    failed = true;
                }
                s
            })
            .collect();

        let response = format!(
            r#"{maybe_modules}{modules_list}{maybe_newline}{maybe_passthrough}{passthrough_list}{maybe_failed}"#,
            maybe_modules = if !modules.is_empty() { "modules: " } else { "" },
            modules_list = modules.join(", "),
            maybe_newline = if !modules.is_empty() { "\n" } else { "" },
            maybe_passthrough = if !passthrough.is_empty() {
                "passthrough: "
            } else {
                ""
            },
            passthrough_list = passthrough.join(", "),
            maybe_failed = if failed {
                "modules marked with * are failed"
            } else {
                ""
            },
        );

        if let Err(e) = event
            .room
            .send(RoomMessageEventContent::text_plain(response))
            .await
        {
            error!("failed sending list response: {e}");
        }
    }
}
