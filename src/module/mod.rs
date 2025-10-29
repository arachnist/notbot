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
//!     // [`ModuleInfo`] for the module. Using the [`ModuleInfo::new`] helper is optional, but
//!     // this function will also take care of channel creation and channel consumer spawning
//!     // for you. For more advanced examples, where this helper is not used, see [`core_starter`].
//!     let wolfram = ModuleInfo::new(
//!         "wolfram",
//!         "calculate something using wolfram alpha",
//!         // Vector [`Acl`] objects representing requirements for triggering the module
//!         vec![],
//!         // [`TriggerType`] for the module. Note how trigger words can be defined in configuration.
//!         TriggerType::Keyword(module_config.keywords.clone()),
//!         // Option of error message prefixes. If processing an event for the module fails, and
//!         // is not `None`, the error message will be posted to the channel.
//!         Some("error getting wolfram response"),
//!         module_config,
//!         processor,
//!     );
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

pub mod chat;
pub mod dispatch;
pub mod modules;
pub mod types;
pub mod workers;

use crate::config::Config;
use crate::klaczdb::KlaczDB;
use crate::tools::ToStringExt;

use tracing::error;

use matrix_sdk::event_handler::EventHandlerHandle;
use matrix_sdk::{Client, Room};

use tokio::sync::mpsc;

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
) -> (EventHandlerHandle, Vec<anyhow::Error>) {
    let klacz = KlaczDB { handle: "main".s() };
    let mut modules: Vec<modules::ModuleInfo> = vec![];
    let mut passthrough_modules: Vec<modules::PassThroughModuleInfo> = vec![];
    let mut workers: Vec<workers::WorkerInfo> = vec![];
    let mut errors: Vec<anyhow::Error> = vec![];

    let (rmod, rpass, rerr) = match crate::notmun::module_starter(mx, config) {
        Ok(r) => r,
        Err(e) => {
            error!("loading notmun failed: {e}");
            (vec![], vec![], vec![e])
        }
    };

    modules.extend(rmod);
    passthrough_modules.extend(rpass);
    errors.extend(rerr);

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
        crate::prom_query::starter,
        crate::points::starter,
    ] {
        match starter(mx, config) {
            Err(e) => {
                error!("module initialization failed fatally: {e}");
                errors.push(e);
            }
            Ok(m) => modules.extend(m),
        };
    }

    #[allow(clippy::single_element_loop, reason = "future functionality")]
    for starter in [crate::kasownik::passthrough, crate::points::passthrough] {
        match starter(mx, config) {
            Err(e) => {
                error!("module initialization failed fatally: {e}");
                errors.push(e);
            }
            Ok(m) => passthrough_modules.extend(m),
        };
    }

    for starter in [
        crate::web::workers,
        crate::spaceapi::workers,
        crate::forgejo::workers,
        crate::gerrit::workers,
        crate::prom_query::workers,
    ] {
        match starter(mx, config) {
            Err(e) => {
                error!("module initialization failed fatally: {e}");
                errors.push(e);
            }
            Ok(m) => workers.extend(m),
        };
    }

    modules.retain(|x| !config.modules_fenced().contains(&x.name));
    passthrough_modules.retain(|x| !config.modules_fenced().contains(&x.0.name));

    modules.extend(chat::core_starter(
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

    (mx.add_event_handler(dispatch::dispatcher), errors)
}
