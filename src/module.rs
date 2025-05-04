use std::ops::Deref;

use crate::config::Config;
use crate::klaczdb::KlaczDB;
use crate::tools::{membership_status, room_name};

use lazy_static::lazy_static;
use prometheus::{opts, register_int_counter_vec, IntCounterVec};
use tracing::{debug, error, trace};

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
    Passthrough,
    Exclusive,
}

pub async fn dispatcher(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    mx: Client,
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

    debug!("dispatching event to modules");
    for (_, module) in run_modules {
        if let Err(e) = dispatch_module(
            true,
            &module,
            klacz_level,
            ev.clone(),
            sender.clone(),
            room.clone(),
            consumer_event.clone(),
        )
        .await
        {
            error!("dispatching module failed: {e}");
        };
    }

    match consumption {
        Consumption::Inclusive | Consumption::Passthrough => {
            debug!("dispatching event to modules");
            let mut run_passthrough_modules: Vec<ModuleInfo> = vec![];

            use TriggerType::*;

            for module in passthrough_modules.iter() {
                // we're in passthrough already, so we don't
                match module.0.trigger {
                    Keyword(ref keywords) => {
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
                if let Err(e) = dispatch_module(
                    false,
                    &module,
                    klacz_level,
                    ev.clone(),
                    sender.clone(),
                    room.clone(),
                    consumer_event.clone(),
                )
                .await
                {
                    error!("dispatching passthrough module failed: {e}");
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
    ev: OriginalSyncRoomMessageEvent,
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
                if required < &klacz_level {
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
            error!("module channel can't accept message: {e}");
            return Ok(());
        }
    };

    MODULE_EVENTS.with_label_values(&[&module.name]).inc();
    trace!("sending event");
    reservation.send(consumer_event);

    Ok(())
}

pub type ModuleStarter = fn(&Client, &Config) -> anyhow::Result<Vec<ModuleInfo>>;
pub type PassthroughModuleStarter =
    fn(&Client, &Config) -> anyhow::Result<Vec<PassThroughModuleInfo>>;

pub fn init_modules(mx: &Client, config: &Config) -> anyhow::Result<EventHandlerHandle> {
    let klacz = KlaczDB { handle: "main" };
    let module_starters: Vec<Box<ModuleStarter>> = vec![];
    let passthrough_module_starters: Vec<Box<PassthroughModuleStarter>> = vec![];
    let mut modules: Vec<ModuleInfo> = vec![];
    let mut passthrough_modules: Vec<PassThroughModuleInfo> = vec![];

    // start moving notmun to becoming a 1st-class citizen
    match crate::notmun::module_starter(mx, config) {
        Err(e) => error!("failed initializing notmun: {e}"),
        Ok(()) => (),
    };

    for starter in [
        crate::klaczdb::starter,
        crate::notmun::starter,
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

    for starter in [crate::kasownik::passthrough] {
        match starter(mx, config) {
            Err(e) => error!("module initialization failed fatally: {e}"),
            Ok(m) => passthrough_modules.extend(m),
        };
    }

    mx.add_event_handler_context(klacz);
    mx.add_event_handler_context(config.clone());
    mx.add_event_handler_context(modules);
    mx.add_event_handler_context(passthrough_modules);

    Ok(mx.add_event_handler(dispatcher))
}
