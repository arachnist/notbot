//! Abstraction over the matrix-rust-sdk event handler system.
//!
//! Provides some structure for defining additional functionality for the bot,
//! as well as functionality not provided by the upstream
//!
//! # Usage
//!
//! ## Keywords:
//! * `help`, `status` - [`help_processor`] - displays help for the bot and known modules, passthrough modules, and workers.
//! * `list`, `list-functions` - [`list_consumer`] - lists known modules, passthrough modules, and workers
//! * `reload` - [`reload_consumer`] - reloads the bot configuration and reinitializes known modules, passthrough modules, and workers
//! * `shutdown`, `die`, `exit` - [`shutdown_consumer`] - causes the bot process to shutdown
//!
//! ## Metrics exposed from this module
//! * [`MODULE_EVENTS`] - `module_event_counts` - number of events consumed, grouped by module.
//! * [`MODULE_ACL_REJECTS`] - `module_acl_failures` - number of ACL checks that failed, preventing an event from being sent to a module, grouped by module
//! * [`MODULE_CHANNEL_FULL`] - `module_channel_full` - number of times the module event channel was full, preventing an event from being sent to a module, grouped by module
//!
//! # Writing modules.
//!
//! This section is directly based on the [`crate::wolfram`] bot module.
//!
//! If you're modifying the bot code directly, you can start with importing [`crate::prelude`] which re-exports types,
//! functions, and macros commonly used throughout the project.
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
//! - `config`: [`crate::config::Config`] - global bot configuration
//!
//! And the function is expected to return [`anyhow::Result`] of a vector of [`ModuleInfo`]s.
//!
//! A good example of a function registering just a single module is [`crate::wolfram`]
//!
//! ```rust
//! use notbot::prelude::*;
//!
//! use notbot::wolfram::{ModuleConfig, processor};
//!
//! pub fn starter(_: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
//!     info!("registering modules");
//!
//!     // object representing module configuration
//!     let module_config: ModuleConfig = config.typed_module_config(module_path!())?;
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
//!         // Channel sender. If the module performs a more sophisticated
//!         // initialization process that failed, the receiver should get dropped and
//!         // channel should be considered closed.
//!         channel: tx,
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
//! use notbot::wolfram::{ModuleConfig, wolfram_alpha};
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
//!     // [`notbot::tools::fetch_and_decode_json`] used here as a helper function to query
//!     // WolframAlpha json api, and decode its response.
//!     let Ok(data) = fetch_and_decode_json::<wolfram_alpha::WolframAlpha>(url).await else {
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
//! # Future plans
//!
//! Trait specifying starter function, configuration object, single-module reload, and maybe some healthcheck function?
//!
//! The module starter now needs to be added to the list of known module starters.
//! This list is, for now, hardcoded, but the plan is to make a dynamic list that can
//! be modified at runtime.

use crate::config::Config;
use crate::klaczdb::KlaczDB;
use crate::tools::{ToStringExt, membership_status, room_name};

use std::ops::{Add, Deref};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::bail;
use futures::Future;
use prometheus::{IntCounterVec, opts, register_int_counter_vec};
use tracing::{debug, error, info, trace, warn};

use matrix_sdk::event_handler::{Ctx, EventHandlerHandle};
use matrix_sdk::ruma::OwnedUserId;
use matrix_sdk::ruma::events::room::message::{
    MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
};
use matrix_sdk::{Client, Room};

use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use askama::Template;
use mlua::Lua;

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
    pub channel: mpsc::Sender<ConsumerEvent>,
    /// A prefix for error messages sent to the channel. If `None`, errors will be only
    /// written to logs.
    pub error_prefix: Option<String>,
}

impl ModuleInfo {
    /// Builds a new `ModuleInfo` object, taking care of creating channels, and spawning the consumer
    pub fn new<C, Fut>(
        name: &str,
        help: &str,
        acl: Vec<Acl>,
        trigger: TriggerType,
        error_prefix: Option<&str>,
        config: C,
        processor: impl Fn(ConsumerEvent, C) -> Fut + Send + 'static,
    ) -> Self
    where
        C: Clone + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let owned_error_prefix = error_prefix.map(str::to_owned);
        let (tx, rx) = mpsc::channel(1);
        Self::spawn_inner(name, owned_error_prefix.clone(), rx, config, processor);

        Self {
            name: name.to_owned(),
            help: help.to_owned(),
            acl,
            trigger,
            channel: tx,
            error_prefix: owned_error_prefix,
        }
    }

    fn spawn_inner<C, Fut>(
        name: &str,
        error_prefix: Option<String>,
        rx: mpsc::Receiver<ConsumerEvent>,
        config: C,
        processor: impl Fn(ConsumerEvent, C) -> Fut + Send + 'static,
    ) where
        C: Clone + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        tokio::task::spawn(Self::consumer(
            rx,
            config,
            error_prefix,
            processor,
            name.to_owned(),
        ));
    }

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
        C: Clone + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        tokio::task::spawn(Self::consumer(
            rx,
            config,
            self.error_prefix.clone(),
            processor,
            self.name.clone(),
        ));
    }

    /// Generic event consumer.
    ///
    /// Consumes events from the `ConsumerEvent` channel and passes them on to the
    /// provided processor function.
    /// # Errors
    /// Will return `Err` if the event channel gets closed.
    pub async fn consumer<C, Fut>(
        mut rx: mpsc::Receiver<ConsumerEvent>,
        config: C,
        error_prefix: Option<String>,
        processor: impl Fn(ConsumerEvent, C) -> Fut,
        name: String,
    ) -> anyhow::Result<()>
    where
        C: Clone + Send + Sync,
        Fut: Future<Output = anyhow::Result<()>>,
    {
        loop {
            let Some(event) = rx.recv().await else {
                warn!("{name} channel closed");
                bail!("channel closed");
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

/// Thin wrapper around `ModuleInfo`
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
    /// module doesn't want this event
    Reject,
    /// module wants this event, but in a passive way; shouldn't be later rejected with ACLs (noisy)
    Inclusive,
    /// module wants this event exclusively, but doesn't mind if passthrough modules catch it as well
    Passthrough,
    /// module wants this event exclusively. mostly for keyworded commands
    Exclusive,
}

/// Main worker object
///
/// Defines things needed from the worker by the help system
/// Workers are intended to be used by background tasks that act on events outside of matrix, even if the do sometimes interact with matrix.
/// Examples of such tasks include a web interface, or `SpaceAPI` observer.
#[derive(Clone, Debug, Template)]
#[template(
    path = "matrix/help-worker.html",
    blocks = ["formatted", "plain"],
)]
pub struct WorkerInfo {
    /// Worker name
    name: String,
    /// Worker help/description
    help: String,
    /// Keyword to show worker status
    keyword: String,
    /// information about the helper module
    helper_module: ModuleInfo,
    /// Handle to trigger worker stopping
    handle: AbortHandle,
}

impl WorkerInfo {
    /// Builds a new `WokrerInfo` object, and spawns the worker and its associated helper module.
    pub fn new<C, Fut>(
        name: &str,
        help: &str,
        keyword: &str,
        mx: Client,
        config: C,
        worker: impl Fn(Client, C) -> Fut + Send + 'static,
    ) -> Self
    where
        C: Clone + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let handle = tokio::task::spawn(worker(mx, config)).abort_handle();
        let (tx, rx) = mpsc::channel(1);
        let helper_module = ModuleInfo {
            name: name.to_owned(),
            help: help.to_owned(),
            acl: vec![],
            trigger: TriggerType::Keyword(vec![keyword.s()]),
            channel: tx,
            error_prefix: None,
        };
        tokio::task::spawn(Self::status_consumer(rx, handle.clone(), name.to_owned()));

        Self {
            name: name.to_owned(),
            help: help.to_owned(),
            keyword: keyword.to_owned(),
            helper_module,
            handle,
        }
    }

    async fn status_consumer(
        mut rx: mpsc::Receiver<ConsumerEvent>,
        worker_handle: AbortHandle,
        name: String,
    ) -> anyhow::Result<()> {
        loop {
            let Some(event) = rx.recv().await else {
                warn!("{name} channel closed");
                worker_handle.abort();
                bail!("channel closed");
            };

            if let Err(e) = event
                .room
                .send(RoomMessageEventContent::text_plain(format!(
                    "worker running: {}",
                    !worker_handle.is_finished()
                )))
                .await
            {
                error!("error sending worker status response: {e}");
            };
        }
    }
}

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
    use Consumption::{Exclusive, Inclusive, Passthrough, Reject};
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
        klacz_level,
        ev: ev.clone(),
        sender: sender.clone(),
        room: room.clone(),
        keyword: keyword.clone(),
        args: remainder,
        lua: lua.deref().clone(),
        klacz: klacz.deref().clone(),
    };

    let mut run_modules: Vec<(Consumption, ModuleInfo)> = vec![];
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
            Reject => continue,
        };
    }

    // restricted prefixes implementation
    if consumption == Consumption::Exclusive {
        // event actually matched a prefix
        if let Some(prefix) = prefix_selected {
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

    trace!("dispatching event to modules");
    for (_, module) in run_modules {
        dispatch_module(
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
    config: Config,
    general: bool,
    module: &ModuleInfo,
    klacz_level: i64,
    sender: OwnedUserId,
    room: Room,
    consumer_event: ConsumerEvent,
) {
    use crate::tools::MembershipStatus::{Active, Inactive, NotAMember, Stoned};
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
                match membership_status(config.capacifier_token(), sender.clone()).await {
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
                match membership_status(config.capacifier_token(), sender.clone()).await {
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

/// Main module initializer
///
/// Initializes "notmun" runtime, sets of main and passthrough modules, and core help,
/// and configuration reloading functionality.
/// This is also where the list of modules to try to initialize lives, see the two for loops
#[allow(
    clippy::cognitive_complexity,
    reason = "Just a few loops, heurestics seem wrong here"
)]
pub fn init_modules(
    mx: &Client,
    config: &Config,
    reload_tx: mpsc::Sender<Room>,
) -> EventHandlerHandle {
    let klacz = KlaczDB { handle: "main" };
    let mut modules: Vec<ModuleInfo> = vec![];
    let mut passthrough_modules: Vec<PassThroughModuleInfo> = vec![];
    let mut workers: Vec<WorkerInfo> = vec![];

    // start moving notmun to becoming a 1st-class citizen
    if let Err(e) = crate::notmun::module_starter(mx, config) {
        error!("failed initializing notmun: {e}");
    } else {
        info!("initialized notmun");
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
        crate::autojoiner::starter,
        crate::forgejo::starter,
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

    for starter in [
        crate::webterface::workers,
        crate::spaceapi::workers,
        crate::forgejo::workers,
        crate::gerrit::workers,
    ] {
        match starter(mx, config) {
            Err(e) => error!("module initialization failed fatally: {e}"),
            Ok(m) => workers.extend(m),
        };
    }

    modules.retain(|x| !config.modules_fenced().contains(&x.name));
    passthrough_modules.retain(|x| !config.modules_fenced().contains(&x.0.name));

    modules.extend(core_starter(
        config,
        reload_tx,
        &modules,
        &passthrough_modules,
        workers.clone(),
    ));

    mx.add_event_handler_context(klacz);
    mx.add_event_handler_context(config.clone());
    mx.add_event_handler_context(modules);
    mx.add_event_handler_context(passthrough_modules);
    mx.add_event_handler_context(workers);

    mx.add_event_handler(dispatcher)
}

/// Initializes help, list, reload, and shutdown modules.
pub fn core_starter(
    config: &Config,
    reload_ev_tx: mpsc::Sender<Room>,
    registered_modules: &[ModuleInfo],
    registered_passthrough_modules: &[PassThroughModuleInfo],
    registered_workers: Vec<WorkerInfo>,
) -> Vec<ModuleInfo> {
    info!("registering modules");
    let mut modules: Vec<ModuleInfo> = vec![];

    let (help_tx, help_rx) = mpsc::channel::<ConsumerEvent>(1);
    let help = ModuleInfo {
        name: "help".s(),
        help: "get help about the bot or its basic functions".s(),
        acl: vec![],
        trigger: TriggerType::Keyword(vec!["help".s(), "status".s()]),
        channel: help_tx,
        error_prefix: None,
    };
    modules.push(help);

    let (list_tx, list_rx) = mpsc::channel::<ConsumerEvent>(1);
    let list = ModuleInfo {
        name: "list".s(),
        help: "get the list of currently registered modules".s(),
        acl: vec![],
        trigger: TriggerType::Keyword(vec!["list".s(), "list-functions".s()]),
        channel: list_tx,
        error_prefix: None,
    };
    modules.push(list);

    let (reload_tx, reload_rx) = mpsc::channel::<ConsumerEvent>(1);
    let reload = ModuleInfo {
        name: "reload".s(),
        help: "reload bot configuration and modules".s(),
        acl: vec![Acl::SpecificUsers(config.admins())],
        trigger: TriggerType::Keyword(vec!["reload".s()]),
        channel: reload_tx,
        error_prefix: None,
    };
    modules.push(reload);

    let (shutdown_tx, shutdown_rx) = mpsc::channel::<ConsumerEvent>(1);
    let shutdown = ModuleInfo {
        name: "shutdown".s(),
        help: "makes the bot process exit, literally".s(),
        acl: vec![Acl::SpecificUsers(config.admins())],
        trigger: TriggerType::Keyword(vec!["shutdown".s(), "die".s(), "exit".s()]),
        channel: shutdown_tx,
        error_prefix: None,
    };
    modules.push(shutdown);

    let mod_manager = ModuleInfo::new(
        "mod_manager",
        "fences off/disables/unfences/enables modules",
        vec![Acl::SpecificUsers(config.admins())],
        TriggerType::Keyword(vec![
            "enable".s(),
            "disable".s(),
            "fence".s(),
            "unfence".s(),
            "disabled".s(),
            "fenced".s(),
        ]),
        Some("action failed"),
        config.clone(),
        mod_manager,
    );
    modules.push(mod_manager);

    // avoids a cyclic reference of help/list holding their own receivers and senders at the same time
    let weak_modules: Vec<WeakModuleInfo> = registered_modules
        .iter()
        .chain(&modules)
        .map(std::convert::Into::into)
        .collect();
    let weak_passthrough: Vec<WeakModuleInfo> = registered_passthrough_modules
        .iter()
        .map(std::convert::Into::into)
        .collect();

    tokio::task::spawn(help_consumer(
        help_rx,
        config.clone(),
        weak_modules.clone(),
        weak_passthrough.clone(),
        registered_workers.clone(),
    ));
    tokio::task::spawn(list_consumer(
        list_rx,
        weak_modules,
        weak_passthrough,
        registered_workers,
    ));
    tokio::task::spawn(reload_consumer(reload_rx, reload_ev_tx));
    tokio::task::spawn(shutdown_consumer(shutdown_rx));

    modules
}

/// Simplified structure for holding information on registered modules. Used by [`help_processor`] and [`list_consumer`], and stores
/// just the information used by those modules.
///
/// [`tokio::sync::mpsc`] channels are kept alive by the runtime for as long as at least one [`tokio::sync::mpsc::Sender`] exists.
/// When the last sender gets dropped, the channel gets closed, and [`tokio::sync::mpsc::Receiver::recv`] returns `None`, allowing a
/// task awaiting on the channel to cleanly exit. An existing [`tokio::sync::mpsc::Sender`] can also be downgraded to
/// [`tokio::sync::mpsc::WeakSender`], which does not count towards the number of references keeping a channel open.
///
/// Help [`help_processor`] and list [`list_consumer`] modules are mostly typical modules, with information about them being held
/// in the [`ModuleInfo`] list, returned by a starter function [`core_starter`], similar to starter functions used by other modules,
/// and added by [`init_modules`] to the matrix client event handler contexts, used by the [`dispatcher`]. This creates the first
/// "strong" reference to the module sender channels, which gets dropped when a new list of modules is added to the event handler
/// context.
///
/// Because of their functionality, help and list modules also "want" to know about themselves. If they held a normal full list of
/// [`ModuleInfo`] objects, this would create a second "strong" reference to the channel objects, including their own senders.
/// This reference would not be affected by the module list being replaced in event handler context map, and thus the event channels -
/// including their own - would be kept open forever. And they also keep a full list of [`PassThroughModuleInfo`] and [`WorkerInfo`]
/// objects. This would create a reference cycle that could not be dropped by the runtime, and kept all tasks spawned by all the modules
/// and workers alive. While most of the bot functionality would stay unaffected, with the only side effect being module objects being
/// kept alive and waiting forever on channels to which nothing would ever send, this meant that workers would stay forever alive as
/// well, possibly keeping external exclusive resources occupied (tcp like listening sockets), and preventing new workers, with new
/// configuration from being started.
///
/// Because of this, a [`WeakModuleInfo`] list is created and passed to help and list modules, with the major difference between it
/// and normal [`ModuleInfo`] being that the channel sender is converted to [`tokio::sync::mpsc::WeakSender`]. Thanks to this, the
/// help and list modules only hold a weak reference to senders for their own channels, meaning that their channels will close, and
/// they will cleanly exit.
///
/// Thus, the object drop order is now:
/// 1. (old) global lists of modules, passthrough modules, and workers
/// 1. "primary" strong event channel references for modules, passthrough modules, and workers
/// 1. event channels for modules and passthrough modules being closed.
/// 1. help and list event consumer loops exiting, along with all the other "normal" modules
/// 1. "secondary" list of workers previously held by help and list modules
/// 1. strong channel references for worker helpers
/// 1. event channels for workers being closed
/// 1. worker helpers event consumer loops exiting
/// 1. workers being stopped when their helper consumer loops exit, by calling [`tokio::task::AbortHandle::abort`] on their handles.
#[derive(Clone, Debug, Template)]
#[template(
    path = "matrix/help-module.html",
    blocks = ["formatted", "plain"],
)]
pub struct WeakModuleInfo {
    /// Name of the module
    pub name: String,
    /// Help for the module
    pub help: String,
    /// Module trigger type
    pub trigger: TriggerType,
    /// Weak reference to the consumer event channel
    pub channel: mpsc::WeakSender<ConsumerEvent>,
}

impl From<&ModuleInfo> for WeakModuleInfo {
    fn from(m: &ModuleInfo) -> Self {
        Self {
            name: m.name.clone(),
            help: m.help.clone(),
            trigger: m.trigger.clone(),
            channel: m.channel.downgrade(),
        }
    }
}

impl From<&PassThroughModuleInfo> for WeakModuleInfo {
    fn from(m: &PassThroughModuleInfo) -> Self {
        Self {
            name: m.0.name.clone(),
            help: m.0.help.clone(),
            trigger: m.0.trigger.clone(),
            channel: m.0.channel.downgrade(),
        }
    }
}

/// Processes bot shutdown request. Singular. This is not a loop, as the process is will exit.
/// # Errors
/// Will return `Err` if event channel is closed.
pub async fn shutdown_consumer(mut rx: mpsc::Receiver<ConsumerEvent>) -> anyhow::Result<()> {
    if rx.recv().await.is_none() {
        warn!("shutdown channel closed");
        bail!("channel closed");
    };

    info!("received process exit request");
    std::process::exit(0);
}

async fn help_consumer(
    mut rx: mpsc::Receiver<ConsumerEvent>,
    config: Config,
    modules: Vec<WeakModuleInfo>,
    passthrough_modules: Vec<WeakModuleInfo>,
    workers: Vec<WorkerInfo>,
) -> anyhow::Result<()> {
    loop {
        let Some(event) = rx.recv().await else {
            warn!("help channel closed");
            bail!("channel closed");
        };

        if let Err(e) = help_processor(
            event.clone(),
            config.clone(),
            modules.clone(),
            passthrough_modules.clone(),
            workers.clone(),
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

#[derive(Template)]
#[template(
    path = "matrix/help-generic.html",
    blocks = ["formatted", "plain"],
)]
struct RenderHelp {
    config: Config,
    modules: Vec<WeakModuleInfo>,
    passthrough: Vec<WeakModuleInfo>,
    workers: Vec<WorkerInfo>,
    source_url: String,
    docs_link: String,
    matrix_contact: String,
}

impl RenderHelp {
    fn failed(&self) -> (usize, usize, usize) {
        (
            self.modules
                .iter()
                .filter(|x| x.channel.upgrade().unwrap().is_closed())
                .count(),
            self.passthrough
                .iter()
                .filter(|x| x.channel.upgrade().unwrap().is_closed())
                .count(),
            self.workers
                .iter()
                .filter(|x| x.handle.is_finished())
                .count(),
        )
    }
}

/// Processes help events. If provided with an argument, will try to match it against a name of known modules, passthrough modules, or
/// workers, to provide more specific help.
/// # Errors
/// Will return error if rendering or sending message fails.
pub async fn help_processor(
    event: ConsumerEvent,
    config: Config,
    modules: Vec<WeakModuleInfo>,
    passthrough: Vec<WeakModuleInfo>,
    workers: Vec<WorkerInfo>,
) -> anyhow::Result<()> {
    let generic = RenderHelp {
        config,
        modules: modules.clone(),
        passthrough: passthrough.clone(),
        workers: workers.clone(),
        source_url: "https://code.hackerspace.pl/ar/notbot".s(),
        docs_link: "https://docs.rs/notbot/latest/notbot/".s(),
        matrix_contact: "@ar:is-a.cat".s(),
    };

    let generic_help = RoomMessageEventContent::text_html(
        generic.as_plain().render()?,
        generic.as_formatted().render()?,
    );

    let Some(args) = event.args else {
        event.room.send(generic_help).await?;
        return Ok(());
    };

    let mut arguments = args.split_whitespace();
    let Some(maybe_module_name) = arguments.next() else {
        event.room.send(generic_help).await?;
        return Ok(());
    };

    let mut specific_response: Option<RoomMessageEventContent> = None;

    for module in modules.iter().chain(&passthrough) {
        if module.name == maybe_module_name {
            let specific_help = RoomMessageEventContent::text_html(
                module.as_plain().render()?,
                module.as_formatted().render()?,
            );
            specific_response = Some(specific_help);
            break;
        };
    }

    for module in workers {
        if module.name == maybe_module_name {
            let specific_help = RoomMessageEventContent::text_html(
                module.as_plain().render()?,
                module.as_formatted().render()?,
            );
            specific_response = Some(specific_help);
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

#[derive(Template)]
#[template(
    path = "matrix/help-list.html",
    blocks = ["formatted", "plain"],
)]
struct RenderList {
    modules: Vec<WeakModuleInfo>,
    passthrough: Vec<WeakModuleInfo>,
    workers: Vec<WorkerInfo>,
}

impl RenderList {
    #[allow(clippy::unused_self, reason = "required by templating engine")]
    fn list_modules(&self, m: &[WeakModuleInfo]) -> (Vec<String>, bool) {
        let mut failed = false;
        (
            m.iter()
                .map(|x| {
                    let mut s = x.name.clone();
                    if x.channel.upgrade().unwrap().is_closed() {
                        s.push('*');
                        failed = true;
                    }
                    s
                })
                .collect(),
            failed,
        )
    }

    fn list_workers(&self) -> (Vec<String>, bool) {
        let mut failed = false;
        (
            self.workers
                .iter()
                .map(|x| {
                    let mut s = x.name.clone();
                    if x.handle.is_finished() {
                        s.push('*');
                        failed = true;
                    };
                    s
                })
                .collect(),
            failed,
        )
    }
}

/// Provides a list of all registered modules, passthrough modules, and workers.
///
/// # Errors
/// Will return `Err` when its own channel gets dropped, or rendering response fails.
pub async fn list_consumer(
    mut rx: mpsc::Receiver<ConsumerEvent>,
    modules: Vec<WeakModuleInfo>,
    passthrough: Vec<WeakModuleInfo>,
    workers: Vec<WorkerInfo>,
) -> anyhow::Result<()> {
    loop {
        let Some(event) = rx.recv().await else {
            warn!("list channel closed");
            bail!("channel closed");
        };

        let render_list = RenderList {
            modules: modules.clone(),
            passthrough: passthrough.clone(),
            workers: workers.clone(),
        };

        let response = RoomMessageEventContent::text_html(
            render_list.as_plain().render()?,
            render_list.as_formatted().render()?,
        );

        if let Err(e) = event.room.send(response).await {
            error!("failed sending list response: {e}");
        }
    }
}

/// Reloads bot configuration, and reinitializes all modules, tasks, and workers, including [`crate::notmun`] state.
/// Is a loop to handle the case where a reload fails due to configuration errors.
/// # Errors
/// Will return `Err` when its own channel gets dropped.
pub async fn reload_consumer(
    mut rx: mpsc::Receiver<ConsumerEvent>,
    reload_tx: mpsc::Sender<Room>,
) -> anyhow::Result<()> {
    loop {
        let Some(event) = rx.recv().await else {
            warn!("reload channel closed");
            bail!("channel closed");
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

/// Enables/disables/fences off/unfences modules.
///
/// # Errors
/// When no module name to disable/fence/enable/unfence is provided, gets passed an unhandled keyword,
/// or sending response fails.
pub async fn mod_manager(event: ConsumerEvent, config: Config) -> anyhow::Result<()> {
    let modname = match event.args {
        None => match event.keyword.as_str() {
            "fenced" | "disabled" => "".s(),
            _ => bail!("no module name provided"),
        },
        Some(m) => m.trim().s(),
    };

    match event.keyword.as_str() {
        "disable" => config.disable_module(modname.clone()),
        "enable" => config.enable_module(&modname),
        "fence" => config.fence_module(modname.clone()),
        "unfence" => config.unfence_module(&modname),
        "disabled" => {
            let disabled = config.modules_disabled();
            let message = format!("disabled modules: {disabled:?}");
            event
                .room
                .send(RoomMessageEventContent::text_plain(message))
                .await?;
            return Ok(());
        }
        "fenced" => {
            let fenced = config.modules_fenced();
            let message = format!("fenced modules: {fenced:?}");
            event
                .room
                .send(RoomMessageEventContent::text_plain(message))
                .await?;
            return Ok(());
        }
        _ => bail!("wtf? wrong keyword passed somehow"),
    }?;

    let message = format!("module {} successfully {}d", modname, event.keyword);

    event
        .room
        .send(RoomMessageEventContent::text_plain(message))
        .await?;

    Ok(())
}
