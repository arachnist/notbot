//! Abstraction over the matrix-rust-sdk event handler system.
//!
//! Provides some structure for defining additional functionality for the bot,
//! as well as functionality not provided by the upstream
//!
//! # Writing modules.
//!
//! Start by including [`crate::prelude`] which re-exports types, functions, and macros used
//! throughout the project.
//!
//! ```
//! use notbot::prelude::*;
//!
//! use serde_json::Value;
//! use urlencoding::encode as uencode;
//! ```
//!
//! Create a struct for configuration of the module, as well as functions for defining any
//! reasonable default values, if applicable.
//!
//! ```
//! use notbot::prelude::*;
//!
//! #[derive(Clone, Deserialize)]
//! pub struct ModuleConfig {
//!     pub app_id: String,
//!     #[serde(default = "default_keywords")]
//!     pub keywords: Vec<String>,
//! }
//!
//! fn default_keywords() -> Vec<String> {
//!     vec!["c".s(), "wolfram".s()]
//! }
//! ```
//!
//! The configuration struct will need to implement `Clone` and `Deserialize` traits, but that is
//! easily achieved with the derive macros.
//! Module configuration is loaded from sections of the global bot configuration, [`crate::config`],
//! which itself is loaded from a toml file, usually `notbot.toml`. The practice is to name the
//! section after the module, for example:
//!
//! ```toml
//! [module."notbot::wolfram"]
//! app_id = "…"
//! keywords = [ "c", "wolfram" ]
//! ```
//!
//! Next step is to define a `starter` function, whose signature is as follows:
//!
//! ```
//! use notbot::prelude::*;
//!
//! pub fn starter(mx: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> { Ok(vec![]) }
//! ```
//!
//! The arguments are:
//! - `mx`: [`matrix_sdk::Client`] - global matrix client
//! - `config`: [`crate::Config`] - global bot configuration
//!
//! And the function is expected to return [`anyhow::Result`] of a vector of [`ModuleInfo`]s.
//!
//! A good example of a function registering just a single module is [`crate::wolfram`]
//!
//! ```rust
//! use notbot::prelude::*;
//!
//! use notbot::wolfram::{ModuleConfig, WolframAlpha, processor};
//!
//! pub fn starter(_: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
//!     info!("registering modules");
//!
//!     // object representing module configuration
//!     let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
//!
//!     // [`tokio::sync::mpsc`] channel for passing events to the module. If the module is not
//!     // expected to take significant amount of time for processing the events, capacity of `1`
//!     // should be enough. If a module won't be able to accept the event, the event will be
//!     // discarded.
//!     let (tx, rx) = mpsc::channel(1);
//!
//!     // [`ModuleInfo`] for the module
//!     let wolfram = ModuleInfo {
//!         name: "wolfram".s(),
//!         help: "calculate something using wolfram alpha".s(),
//!         // Vector [`Acl`] objects representing requirements for triggering the module
//!         acl: vec![],
//!         // [`TriggerType`] for the module. Note how triggers can be defined in configuration.
//!         trigger: TriggerType::Keyword(module_config.keywords.clone()),
//!         // Option of channel sender. If the module performs a more sophisticated
//!         // initialization process that failed, you can pass None here.
//!         channel: Some(tx),
//!         // Option of error message prefixes. If processing an event for the module fails, and
//!         // is not `None`, the error message will be posted to the channel.
//!         error_prefix: Some("error getting wolfram response".s()),
//!     };
//!     // Actually starting the module. Using this function is optional, and you can start an event
//!     // processing task your own way. See [`core_starter`] for an example of that, where the
//!     // event consumers and processors for `help`, `list`, and `reload` functionality take
//!     // additional arguments.
//!     // `processor`, as passed to the [`ModuleInfo::spawn`] function, is expected to take a
//!     // single [`ConsumerEvent`] and module configuration as arguments.
//!     wolfram.spawn(rx, module_config, processor);
//!
//!     // Return the list of registered modules.
//!     // The module list can also be constructed dynamically, and appended with each registered
//!     // module.
//!     // ```
//!     // let mut modules: Vec<ModuleInfo> = vec![];
//!     // ...
//!     // modules.push(wolfram);
//!     // ```
//!     Ok(vec![wolfram])
//! }
//! ```
//!
//! Modules need to process events sent to them. The typical checks, like access control, or keyword
//! matching is handled by [`dispatcher`] and [`dispatch_module`] functions, so the module only
//! needs to handle things specific to it:
//!
//! ```rust
//! use notbot::prelude::*;
//!
//! use serde_json::Value;
//! use urlencoding::encode as uencode;
//!
//! use notbot::wolfram::{ModuleConfig, WolframAlpha};
//!
//! fn default_keywords() -> Vec<String> {
//!     vec!["c".s(), "wolfram".s()]
//! }
//!
//! pub async fn processor(event: ConsumerEvent, config: ModuleConfig) -> anyhow::Result<()> {
//!     // check if the user actually passed any extra arguments
//!     let Some(text_query) = event.args else {
//!         event
//!             .room
//!             .send(RoomMessageEventContent::text_plain(
//!                 "missing argument: query",
//!             ))
//!             .await?;
//!         return Ok(());
//!     };
//!
//!     // encode the query using [`urlencoding::encode`]
//!     let query = uencode(text_query.as_str());
//!
//!     // construct the http query string
//!     let url: String = "http://api.wolframalpha.com/v2/query?input=".to_owned()
//!         + query.as_ref()
//!         + "&appid="
//!         + config.app_id.as_str()
//!         + "&output=json";
//!
//!     // [`crate::tools::fetch_and_decode_json`] used here as a helper function to query
//!     // WolframAlpha json api, and decode its response.
//!     let Ok(data) = fetch_and_decode_json::<WolframAlpha>(url).await else {
//!         bail!("couldn't fetch data from wolfram")
//!     };
//!
//!     // Validate the returned data beyond what deserialize json can do
//!     if !data.queryresult.success || data.queryresult.numpods == 0 {
//!         event
//!             .room
//!             .send(RoomMessageEventContent::text_plain("no results"))
//!             .await?;
//!     };
//!
//!     // Prepare response sent back to the room:
//!     let mut response_parts: Vec<String> = vec![];
//!     for pod in data.queryresult.pods {
//!         if pod.primary.is_some_and(|x| x) {
//!             response_parts.push(pod.title + ": " + pod.subpods[0].plaintext.as_str());
//!         }
//!     }
//!
//!     // Actually send the response
//!     event
//!         .room
//!         .send(RoomMessageEventContent::text_plain(
//!             response_parts.join("\n"),
//!         ))
//!         .await?;
//!
//!     Ok(())
//! }
//! ```
//!
//! The module starter now needs to be added to the list of known module starters.
//! This list is, for now, hardcoded, but the plan is to make a dynamic list that can
//! be modified at runtime.

use crate::config::Config;
use crate::klaczdb::KlaczDB;
use crate::tools::{membership_status, room_name, ToStringExt};

use std::ops::Deref;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::de;
use anyhow::bail;
use futures::Future;
use prometheus::{opts, register_int_counter_vec, IntCounterVec};
use serde_derive::Deserialize;
use tracing::{debug, error, info, trace};

use matrix_sdk::event_handler::{Ctx, EventHandlerHandle};
use matrix_sdk::ruma::events::room::message::{
    MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
};
use matrix_sdk::ruma::OwnedUserId;
use matrix_sdk::{Client, Room};

use tokio::sync::mpsc;

use mlua::Lua;

static MODULE_EVENTS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "module_event_counts",
            "Number of events a module has consumed"
        ),
        &["event"]
    )
    .unwrap()
});

/// An event object passed to modules.
///
/// For modules consuming text-like events, this should contain everything that's needed.
#[derive(Clone)]
pub struct ConsumerEvent {
    /// "klacz" permission level of the event sender, defined on a room/sender pair.
    pub klacz_level: i64,
    /// full original event from matrix-rust-sdk
    pub ev: OriginalSyncRoomMessageEvent,
    /// convienience field for event sender
    pub sender: OwnedUserId,
    /// room in which the event originated
    pub room: Room,
    /// first word (whitespace deliminated) of text in the event content after the prefix
    pub keyword: String,
    /// possible rest of the text in the event after the keyword
    pub args: Option<String>,
    /// lua interpreter, pre-configured for running [notmun](https://code.hackerspace.pl/ar/notmun) modules and functions.
    pub lua: Lua,
    /// [klacz](https://code.hackerspace.pl/hswaw/klacz) database object, providing convienient access to the database contents
    pub klacz: KlaczDB,
}

/// Main module object.
///
/// Defines the things needed from the module by the dispatcher and help system.
#[derive(Clone, Debug)]
pub struct ModuleInfo {
    /// Module name
    pub name: String,
    /// Short help/description of the module
    pub help: String,
    /// ACLs required for the module. All that are defined for the module must be satisfied.
    pub acl: Vec<Acl>,
    /// Type of module trigger
    pub trigger: TriggerType,
    /// mpsc channel for sending events to the module
    pub channel: Option<mpsc::Sender<ConsumerEvent>>,
    /// A prefix for error messages sent to the channel. If `None`, errors will be only
    /// written to logs.
    pub error_prefix: Option<String>,
}

impl ModuleInfo {
    /// Convenience function to spawn generic event channel consumer.
    ///
    /// Spawns a new tokio task dedicated to receiving events from the module mpsc
    /// channel, and calling the event processor
    pub fn spawn<C, Fut>(
        &self,
        rx: mpsc::Receiver<ConsumerEvent>,
        config: C,
        processor: impl Fn(ConsumerEvent, C) -> Fut + Send + 'static,
    ) where
        C: de::DeserializeOwned + Clone + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let prefix = self.error_prefix.clone();
        tokio::task::spawn(Self::consumer(
            rx,
            config.clone(),
            prefix.clone(),
            processor,
        ));
    }

    /// Generic event consumer.
    ///
    /// Consumes events from the ConsumerEvent channel and passes them on to the
    /// provided processor function.
    pub async fn consumer<C, Fut>(
        mut rx: mpsc::Receiver<ConsumerEvent>,
        config: C,
        error_prefix: Option<String>,
        processor: impl Fn(ConsumerEvent, C) -> Fut,
    ) -> anyhow::Result<()>
    where
        C: de::DeserializeOwned + Clone + Send + Sync,
        Fut: Future<Output = anyhow::Result<()>>,
    {
        loop {
            let event = match rx.recv().await {
                Some(e) => e,
                None => {
                    error!("channel closed, goodbye! :(");
                    bail!("channel closed");
                }
            };

            if let Err(e) = processor(event.clone(), config.clone()).await {
                error!("error processing event: {e}");
                if let Some(ref prefix) = error_prefix {
                    if let Err(ee) = event
                        .room
                        .send(RoomMessageEventContent::text_plain(format!(
                            "{prefix}: {e}"
                        )))
                        .await
                    {
                        error!("error when sending event response: {ee}");
                    }
                };
            }
        }
    }
}

/// Thin wrapper around ModuleInfo
///
/// Exists because the matrix-rust-sdk can only hold one extra context object per type.
#[derive(Clone)]
pub struct PassThroughModuleInfo(pub ModuleInfo);

/// Function signature for Decider function for non-keyword modules.
///
/// Returns Consumption to indicate whether or not the module will want to consume
/// the event, and in what exclusivity manner.
pub type CatchallDecider = fn(
    klaczlevel: i64,
    sender: OwnedUserId,
    room: &Room,
    content: &RoomMessageEventContent,
    config: &Config,
) -> anyhow::Result<Consumption>;

/// Possible ways to trigger a module
#[derive(Clone, Debug)]
pub enum TriggerType {
    /// Module responds to pre-defined keywords.
    Keyword(Vec<String>),
    /// Module uses a function to accept or reject modules using an arbitrary function.
    Catchall(CatchallDecider),
}

/// Access control lists for modules.
///
/// Define conditions required for an event to be passed to the module.
/// All defined conditions must be satisfied.
#[derive(Clone, Debug)]
pub enum Acl {
    /// User must be an active member of the Warsaw Hackerspace.
    ActiveHswawMember,
    /// User must be a past or present known member of the Warsaw Hackerspace.
    MaybeInactiveHswawMember,
    /// User is one of the pre-defined known users.
    SpecificUsers(Vec<String>),
    /// Event was sent to a specific room.
    Room(Vec<String>),
    /// User has a perimission level defined in the `oof`/`ood`/`klacz` database that is
    /// not lower than the defined value.
    KlaczLevel(i64),
    /// User comes from one of pre-defined homeserers.
    Homeserver(Vec<String>),
}

/// Event consumption manners for modules.
///
/// Indicate whether or not a module will consume an incoming event, and in what manner.
/// Modules triggered by keywords will always consume exclusively.
/// If, due to configuration, multiple modules were to consume an event exclusively, the
/// first one checked wins.
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

/// Main event dispatcher.
///
/// Handles incoming text-like events, checks if they're not our own echoed back events,
/// checks whether they match a prefix and keyword, handles consumption levels logic for
/// regular and passthrough (rejection only) modules, and modules and events to [`dispatch_module`]
#[allow(clippy::too_many_arguments)]
pub async fn dispatcher(
    ev: OriginalSyncRoomMessageEvent,
    client: Client,
    room: Room,
    config: Ctx<Config>,
    modules: Ctx<Vec<ModuleInfo>>,
    passthrough_modules: Ctx<Vec<PassThroughModuleInfo>>,
    klacz: Ctx<KlaczDB>,
    lua: Ctx<Lua>,
) -> anyhow::Result<()> {
    use Consumption::*;

    let sender: OwnedUserId = ev.sender.clone();

    if config.user_id() == sender
        || config.ignored().contains(&sender.to_string())
        || client.user_id().unwrap() == sender
    {
        return Ok(());
    }

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
            config.clone(),
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
                    config.clone(),
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

/// Actual module event dispatcher.
///
/// Checks ACLs, responds accordingly if ACLs fail, checks if event can be sent to
/// the module, and sends the event.
pub async fn dispatch_module(
    config: Config,
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
            room.send(RoomMessageEventContent::text_plain(response))
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

/// Main module initializer
///
/// Initializes "notmun" runtime, sets of main and passthrough modules, and core help,
/// and configuration reloading functionality.
/// This is also where the list of modules to try to initialize lives, see the two for loops
pub fn init_modules(
    mx: &Client,
    config: &Config,
    reload_tx: mpsc::Sender<Room>,
) -> anyhow::Result<EventHandlerHandle> {
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
        crate::alerts::starter,
    ] {
        match starter(mx, config) {
            Err(e) => error!("module initialization failed fatally: {e}"),
            Ok(m) => modules.extend(m),
        };
    }

    for starter in [crate::kasownik::passthrough, crate::notmun::passthrough] {
        match starter(mx, config) {
            Err(e) => error!("module initialization failed fatally: {e}"),
            Ok(m) => passthrough_modules.extend(m),
        };
    }

    match core_starter(
        config,
        reload_tx,
        modules.clone(),
        passthrough_modules.clone(),
    ) {
        Err(e) => error!("core modules module initialization failed fatally: {e}"),
        Ok(m) => modules.extend(m),
    };

    mx.add_event_handler_context(klacz);
    mx.add_event_handler_context(config.clone());
    mx.add_event_handler_context(modules);
    mx.add_event_handler_context(passthrough_modules);

    Ok(mx.add_event_handler(dispatcher))
}

pub fn core_starter(
    config: &Config,
    reload_ev_tx: mpsc::Sender<Room>,
    mut registered_modules: Vec<ModuleInfo>,
    registered_passthrough_modules: Vec<PassThroughModuleInfo>,
) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering modules");
    let mut modules: Vec<ModuleInfo> = vec![];
    let reloader_config: ReloaderConfig =
        config.module_config_value("notbot::reloader")?.try_into()?;

    let (help_tx, help_rx) = mpsc::channel::<ConsumerEvent>(1);
    let help = ModuleInfo {
        name: "help".s(),
        help: "get help about the bot or its basic functions".s(),
        acl: vec![],
        trigger: TriggerType::Keyword(vec!["help".s(), "status".s()]),
        channel: Some(help_tx),
        error_prefix: None,
    };
    modules.push(help);

    let (list_tx, list_rx) = mpsc::channel::<ConsumerEvent>(1);
    let list = ModuleInfo {
        name: "list".s(),
        help: "get the list of currently registered modules".s(),
        acl: vec![],
        trigger: TriggerType::Keyword(vec!["list".s(), "list-functions".s()]),
        channel: Some(list_tx),
        error_prefix: None,
    };
    modules.push(list);

    let (reload_tx, reload_rx) = mpsc::channel::<ConsumerEvent>(1);
    let reload = ModuleInfo {
        name: "reload".s(),
        help: "reload bot configuration and modules".s(),
        acl: vec![Acl::SpecificUsers(reloader_config.admins)],
        trigger: TriggerType::Keyword(vec!["reload".s()]),
        channel: Some(reload_tx),
        error_prefix: None,
    };
    modules.push(reload);

    registered_modules.extend(modules.clone());
    tokio::task::spawn(help_consumer(
        help_rx,
        config.clone(),
        registered_modules.clone(),
        registered_passthrough_modules.clone(),
    ));
    tokio::task::spawn(list_consumer(
        list_rx,
        registered_modules.clone(),
        registered_passthrough_modules.clone(),
    ));
    tokio::task::spawn(reload_consumer(reload_rx, reload_ev_tx));

    Ok(modules)
}

pub async fn help_consumer(
    mut rx: mpsc::Receiver<ConsumerEvent>,
    config: Config,
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
            config.clone(),
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
                error!("error while sending response: {e}");
            };
        }
    }
}

pub async fn help_processor(
    event: ConsumerEvent,
    config: Config,
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
        let prefixes = config.prefixes();
        let plain = format!(
            r#"this is the notbot; source {source_url}; configured prefixes: {prefixes:?}
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
            r#"this is the notbot; source {source_url}; configured prefixes: {prefixes:?}<br />
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
            let keywords = match module.trigger {
                TriggerType::Catchall(_) => "".s(),
                TriggerType::Keyword(v) => format!(
                    "\nkeyword{maybe_s}: {kws}",
                    maybe_s = if v.len() > 1 { "s" } else { "" },
                    kws = v.join(", ")
                ),
            };
            let response = format!(
                r#"module {name} is {status}; help: {help}{keywords}"#,
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

pub async fn list_consumer(
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

pub async fn reload_consumer(
    mut rx: mpsc::Receiver<ConsumerEvent>,
    reload_tx: mpsc::Sender<Room>,
) -> anyhow::Result<()> {
    loop {
        let event = match rx.recv().await {
            Some(e) => e,
            None => {
                error!("channel closed, goodbye! :(");
                bail!("channel closed");
            }
        };

        let reservation = match reload_tx.clone().try_reserve_owned() {
            Ok(r) => r,
            Err(e) => {
                error!("reloader can't accept trigger: {e}");
                continue;
            }
        };

        reservation.send(event.room);
    }
}

#[derive(Clone, Deserialize)]
pub struct ReloaderConfig {
    admins: Vec<String>,
}
