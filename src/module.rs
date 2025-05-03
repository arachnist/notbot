use crate::config::Config;
use crate::klaczdb::KlaczDB;

use lazy_static::lazy_static;
use prometheus::{opts, register_int_counter_vec, IntCounterVec};
use tracing::{debug, error};

use matrix_sdk::ruma::events::room::message::{
    MessageFormat, MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
};
use matrix_sdk::ruma::OwnedUserId;
use matrix_sdk::{Client, Room};

use tokio::sync::mpsc;

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

pub type ConsumerEvent = (
    i64, // klacz level
    OriginalSyncRoomMessageEvent,
    OwnedUserId,
    Room,
);

#[derive(Clone)]
pub struct ModuleInfo {
    pub name: String,
    pub help: String,
    pub acl: ACL,
    pub trigger: TriggerType,
    pub channel: mpsc::Sender<ConsumerEvent>,
}

// distinct from generic ModuleInfo for legal purposes only
#[derive(Clone)]
pub struct PassThroughModuleInfo(ModuleInfo);

pub type CatchallDecider = fn(
    klaczlevel: i64,
    sender: OwnedUserId,
    room: &Room,
    content: &RoomMessageEventContent,
) -> anyhow::Result<Consumption>;

#[derive(Clone)]
pub enum TriggerType {
    Keyword(Vec<String>),
    Catchall(CatchallDecider),
}

#[derive(Clone)]
pub enum ACL {
    KlaczLevel(i64),
    Homeserver(Vec<String>),
    None,
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone)]
pub enum Consumption {
    // module doesn't want this event
    Reject,
    // module wants this event, but in a passive way; shouldn't be later rejected with ACLs (noisy)
    Inclusive,
    Passthrough,
    Exclusive,
}

pub(crate) async fn dispatch(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    mx: Client,
    config: Config,
    modules: Vec<ModuleInfo>,
    passthrough_modules: Vec<PassThroughModuleInfo>,
    klacz: KlaczDB,
) -> anyhow::Result<()> {
    use Consumption::*;

    let sender: OwnedUserId = ev.sender.clone();

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

    let klacz_level = match klacz.get_level(&room, &sender).await {
        Ok(level) => level,
        Err(e) => {
            error!("error getting klacz permission level: {e}");
            0
        }
    };

    let mut args = text.trim_start().splitn(2, [' ', ' ', '\t']);
    let first = args.next();

    let (keyword, remainder): (String, Option<String>) = {
        match first {
            None => (String::new(), None),
            Some(word) => {
                let (mut kw_candidate, mut remainder_candidate) = (String::new(), None);
                for prefix in config.prefixes() {
                    match prefix.len() {
                        1 => {
                            match word.strip_prefix(prefix.as_str()) {
                                None => continue,
                                Some(w) => {
                                    kw_candidate = w.to_string();
                                    remainder_candidate = args.next().map(|s| s.to_string());
                                    break;
                                }
                            }
                        },
                        // meme command case
                        2.. => {
                            if word == prefix {
                                if let Some(shifted_text) = args.next() {
                                    let mut shifted_args = shifted_text.trim_start().splitn(2, [' ', ' ', '\t']);
                                    if let Some(second) = shifted_args.next() {
                                        kw_candidate = second.to_string();
                                        remainder_candidate = shifted_args.next().map(|s| s.to_string());
                                    };
                                };
                                break;
                            };
                        },
                        0 => continue,
                    };
                };

                (kw_candidate, remainder_candidate)
            },
        }
    };

    let mut run_modules: Vec<(Consumption, ModuleInfo)> = vec![];
    let mut consumption = Inclusive;

    // go through all the modules first to figure out consumption priority
    for module in modules {
        use TriggerType::*;

        let module_consumption: Consumption = match module.trigger {
            Keyword(ref keywords) => {
                if !keyword.is_empty() && keywords.contains(&keyword) {
                    continue;
                };

                Exclusive
            }
            Catchall(fun) => match fun(klacz_level, sender.clone(), &room, &ev.content) {
                Err(e) => {
                    error!("{} decider returned error: {e}", module.name);
                    continue;
                }
                Ok(Reject) => continue,
                Ok(c) => c,
            },
        };

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
                    run_modules.push((module_consumption, module));
                }
            }
            Reject => {
                error!(
                    "module {} somehow returned Reject consumption without skipping the loop",
                    module.name
                );
                continue;
            }
        };
    }

    for (_, module) in run_modules {
        if let Err(e) = dispatch_module(
            true,
            &module,
            klacz_level,
            ev.clone(),
            sender.clone(),
            room.clone(),
        )
        .await
        {
            error!("dispatching module failed: {e}");
        };
    }

    match consumption {
        Consumption::Inclusive | Consumption::Passthrough => {
            debug!("running passthrough modules");
            let mut run_passthrough_modules: Vec<ModuleInfo> = vec![];

            use TriggerType::*;

            for module in passthrough_modules {
                // we're in passthrough already, so we don't 
                match module.0.trigger {
                    Keyword(ref keywords) => {
                        // handle that on init?
                        error!("can't have keyword modules in passthrough!");
                        continue;
                    }
                    Catchall(fun) => match fun(klacz_level, sender.clone(), &room, &ev.content) {
                        Err(e) => {
                            error!("{} decider returned error: {e}", module.0.name);
                            continue;
                        }
                        Ok(Reject) => continue,
                        Ok(_) => run_passthrough_modules.push(module.0),
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
) -> anyhow::Result<()> {
    use ACL::*;

    match &module.acl {
        None => (),
        KlaczLevel(required) => {
            if required < &klacz_level {
                if general {
                    room.send(RoomMessageEventContent::text_plain(
                        "I'm sorry Dave, I'm afraid I can't do that",
                    ))
                    .await?;
                };
                return Ok(());
            }
        }
        Homeserver(homeservers) => {
            if !homeservers.contains(&sender.server_name().to_string()) {
                if general {
                    room.send(RoomMessageEventContent::text_plain(
                        "I'm sorry Dave, I'm afraid I can't do that",
                    ))
                    .await?;
                };
                return Ok(());
            }
        }
    };

    let reservation = match module.channel.clone().try_reserve_owned() {
        Ok(r) => r,
        Err(e) => {
            error!("module channel can't accept message: {e}");
            return Ok(());
        }
    };
    let consumer_event: ConsumerEvent = (klacz_level, ev.clone(), sender.clone(), room.clone());
    MODULE_EVENTS.with_label_values(&[&module.name]).inc();
    reservation.send(consumer_event);

    Ok(())
}

/*
pub(crate) async fn init_modules(mx: &Client, config: &Config) -> anyhow::Result<()> {
    use TriggerType::*;

    let mut modules: Vec<ModuleInfo> = vec![];

    let m = modules.first().unwrap();

    let decision: bool = match &m.trigger {
        Keyword(keywords) => true,
        Catchall(fun) => false,
    };

    Ok(())
}
*/
