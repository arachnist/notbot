//! Main dispatcher of events to modules.

use super::types::{Consumption, TriggerType, Acl, ConsumerEvent};
use super::modules::{ModuleInfo, PassThroughModuleInfo};
use super::workers::WorkerInfo;

use crate::config::Config;
use crate::tools::{membership_status, room_name};
use crate::klaczdb::KlaczDB;

use std::ops::{Add, Deref};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing::{debug, error, trace};

use matrix_sdk::event_handler::Ctx;
use matrix_sdk::ruma::OwnedUserId;
use matrix_sdk::ruma::events::room::message::{
    MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
};
use matrix_sdk::Room;

use mlua::Lua;

use prometheus::{
    IntCounterVec, opts, register_int_counter_vec,
};

/// Number of events consumed, grouped by module
pub static MODULE_EVENTS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "module_event_counts",
            "Number of events a module has consumed"
        ),
        &["module"]
    )
    .unwrap()
});

/// Number acl checks failed, grouped by module
pub static MODULE_ACL_REJECTS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "module_acl_failures",
            "Number acl checks failed, grouped by module"
        ),
        &["module"]
    )
    .unwrap()
});

/// Number of times an attempt was made to pass an event to a module, but the module channel was full.
pub static MODULE_CHANNEL_FULL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "module_channel_full",
            "Number of events a module did not consume due to event channel being full"
        ),
        &["module"]
    )
    .unwrap()
});

/// Main event dispatcher.
///
/// Handles incoming text-like events, checks if they're not our own echoed back events,
/// checks whether they match a prefix and keyword, handles consumption levels logic for
/// regular and passthrough (rejection only) modules, and modules and events to [`dispatch_module`]
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub async fn dispatcher(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    config: Ctx<Config>,
    modules: Ctx<Vec<ModuleInfo>>,
    passthrough_modules: Ctx<Vec<PassThroughModuleInfo>>,
    workers: Ctx<Vec<WorkerInfo>>,
    klacz: Ctx<KlaczDB>,
    lua: Ctx<Lua>,
) {
    use Consumption::{CommandNotFound, Exclusive, Inclusive, Passthrough, Reject};
    use TriggerType::{Catchall, Keyword};

    let Some(ev_ts) = ev.origin_server_ts.to_system_time() else {
        error!("event timestamp couldn't get parsed to system time");
        return;
    };

    if ev_ts.add(Duration::from_secs(10)) < SystemTime::now() {
        debug!("received too old event: {ev_ts:?}");
        return;
    };

    let sender: OwnedUserId = ev.sender.clone();

    if config.user_id() == sender || config.ignored().contains(&sender.to_string()) {
        return;
    }

    // filter unhandled message types
    match ev.content.msgtype {
        MessageType::Text(_) | MessageType::Notice(_) => (),
        _ => return,
    }

    let text = ev.content.body();

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

    let mut prefixes_all: Vec<String> = config.prefixes();
    if let Some(hash) = config.prefixes_restricted() {
        prefixes_all.extend(hash.keys().map(std::borrow::ToOwned::to_owned));
    };
    let mut prefix_selected: Option<String> = None;

    trace!("cursed prefix matching");
    let (keyword, remainder): (String, Option<String>) = {
        first.map_or_else(
            || (String::new(), None),
            #[allow(clippy::cognitive_complexity)]
            |word| {
                trace!("first word exists: {word}");
                let (mut kw_candidate, mut remainder_candidate) = (String::new(), None);
                for prefix in prefixes_all {
                    trace!("trying prefix: {prefix}");
                    match prefix.len() {
                        1 => match word.strip_prefix(prefix.as_str()) {
                            None => continue,
                            Some(w) => {
                                kw_candidate = w.to_string();
                                remainder_candidate =
                                    args.next().map(std::string::ToString::to_string);
                                trace!("selected prefix: {prefix}");
                                prefix_selected = Some(prefix);
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
                                        remainder_candidate = shifted_args
                                            .next()
                                            .map(std::string::ToString::to_string);
                                    };
                                };
                                trace!("selected prefix: {prefix}");
                                prefix_selected = Some(prefix);
                                break;
                            };
                        }
                        0 => continue,
                    };
                }

                (kw_candidate, remainder_candidate)
            },
        )
    };

    let consumer_event = ConsumerEvent {
        ev: ev.clone(),
        sender: sender.clone(),
        room: room.clone(),
        keyword: keyword.clone(),
        args: remainder,
        lua: lua.deref().clone(),
    };

    let mut run_modules: Vec<(Consumption, ModuleInfo)> = vec![];
    let mut command_not_found: Option<ModuleInfo> = None;
    let mut consumption = Inclusive;

    // go through all the modules first to figure out consumption priority
    // for the purpose of matching keywords, workers are just sparkling modules.
    trace!("figuring out event consumption priority");
    for module in modules
        .iter()
        .chain(workers.iter().map(|w| &w.helper_module))
    {
        trace!("considering module: {}", module.name);
        if module.channel.is_closed() {
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
                consumption = module_consumption;
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
                };

                run_modules.push((module_consumption, module.clone()));
            }
            // normal module registering for command-not-found? sure
            CommandNotFound => {
                trace!("registering as command-not-found: {}", module.name);
                if command_not_found.is_none() {
                    command_not_found = Some(module.clone());
                };
            }
            Reject => continue,
        };
    }

    // restricted prefixes implementation
    if consumption == Consumption::Exclusive {
        // event actually matched a prefix
        if let Some(prefix) = prefix_selected.clone() {
            // restricted prefixes map is defined
            if let Some(map) = config.prefixes_restricted() {
                // the matched prefix is on the list
                if let Some(list) = map.get(&prefix) {
                    // the list should contain exactly one module anyway
                    for (_, module) in &run_modules {
                        // if any module we're trying to run is not on the list, bail.
                        if !list.contains(&module.name) {
                            return;
                        }
                    }
                }
            }
        }
    }

    let rclient = match reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(h) => h,
        Err(e) => {
            error!("couldn't create a basic http client: {e}");
            return;
        }
    };

    trace!("dispatching event to modules");
    for (_, module) in run_modules {
        dispatch_module(
            &rclient,
            config.clone(),
            true,
            &module,
            klacz_level,
            sender.clone(),
            room.clone(),
            consumer_event.clone(),
        )
        .await;
    }

    match consumption {
        Consumption::Inclusive | Consumption::Passthrough => {
            trace!("dispatching event to passthrough modules");
            let mut run_passthrough_modules: Vec<ModuleInfo> = vec![];

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
                        Ok(CommandNotFound) => {
                            trace!("registering command not found: {}", module.0.name);
                            if command_not_found.is_none() {
                                command_not_found = Some(module.0.clone());
                            };
                        }
                        Ok(_) => run_passthrough_modules.push(module.0.clone()),
                    },
                };
            }

            for module in run_passthrough_modules {
                if module.channel.is_closed() {
                    debug!("failed module, skipping");
                    continue;
                };

                dispatch_module(
                    &rclient,
                    config.clone(),
                    false,
                    &module,
                    klacz_level,
                    sender.clone(),
                    room.clone(),
                    consumer_event.clone(),
                )
                .await;
            }

            if prefix_selected.is_some_and(|e| config.prefixes().contains(&e)) {
                if let Some(command_not_found) = command_not_found {
                    dispatch_module(
                        &rclient,
                        config.clone(),
                        false,
                        &command_not_found,
                        klacz_level,
                        sender.clone(),
                        room.clone(),
                        consumer_event.clone(),
                    )
                    .await;
                };
            };
        }
        _ => {
            debug!("skipping passthrough modules");
        }
    };
}

/// Actual module event dispatcher.
///
/// Checks ACLs, responds accordingly if ACLs fail, checks if event can be sent to
/// the module, and sends the event.
#[allow(clippy::too_many_lines)]
pub async fn dispatch_module(
    rclient: &reqwest::Client,
    config: Config,
    general: bool,
    module: &ModuleInfo,
    klacz_level: i64,
    sender: OwnedUserId,
    room: Room,
    consumer_event: ConsumerEvent,
) {
    use crate::tools::MembershipStatus::{Active, Inactive, NotAMember};
    use Acl::{
        ActiveHswawMember, Homeserver, KlaczLevel, MaybeInactiveHswawMember, Room, SpecificUsers,
    };

    trace!("dispatching module: {}", module.name);

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
                match membership_status(rclient, config.capacifier_token(), sender.clone()).await {
                    Err(e) => {
                        error!("checking membership for {sender} failed: {e}");
                        failed = true;
                    }
                    Ok(status) => match status {
                        Inactive(_) | NotAMember => failed = true,
                        // kasownik, and - by extension - the board, has authority on who is an active member
                        Active(_, _) => (),
                    },
                }
            }
            MaybeInactiveHswawMember => {
                match membership_status(rclient, config.capacifier_token(), sender.clone()).await {
                    Err(e) => {
                        error!("checking membership for {sender} failed: {e}");
                        failed = true;
                    }
                    Ok(status) => match status {
                        NotAMember => failed = true,
                        Inactive(_) | Active(_, _) => (),
                    },
                }
            }
        };

        if failed {
            break;
        }
    }

    if failed {
        MODULE_ACL_REJECTS.with_label_values(&[&module.name]).inc();
        if general {
            let mut response = "busy figuring out why time behaves weirdly";
            let options = config.acl_deny();
            // matrix-rust-sdk doesn't like it if we use rand::rng() here in
            // an actually useful (producing random results) way.
            // the trait `EventHandler<_, _>` is not implemented for fn item …
            // so i'm doing the next best thing: milis % vector length
            if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
                let milis = now.as_millis();
                // FIXME: sketchy AF
                let chosen_idx: usize = milis as usize % config.acl_deny().len();
                if let Some(option) = options.get(chosen_idx) {
                    response = option;
                };
            };
            if let Err(e) = room
                .send(RoomMessageEventContent::text_plain(response))
                .await
            {
                error!("sending acl failure response failed: {e}");
            }
        };
        return;
    };

    // also filter out fenced modules, so they don't get sent any events while still running
    if config.modules_disabled().contains(&module.name)
        || config.modules_fenced().contains(&module.name)
    {
        trace!("module disabled: {}", module.name);
        return;
    }

    trace!("attempting to reserve channel space");
    let reservation = match module.channel.clone().try_reserve_owned() {
        Ok(r) => r,
        Err(e) => {
            MODULE_CHANNEL_FULL.with_label_values(&[&module.name]).inc();
            error!("module {} channel can't accept message: {e}", module.name);
            return;
        }
    };

    MODULE_EVENTS.with_label_values(&[&module.name]).inc();
    trace!("sending event");
    reservation.send(consumer_event);
}
