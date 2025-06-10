//! Types used for handling modules.

use super::modules::{ModuleInfo, PassThroughModuleInfo};
use super::workers::WorkerInfo;

use crate::config::Config;

use matrix_sdk::ruma::events::room::message::{OriginalSyncRoomMessageEvent, RoomMessageEventContent};
use matrix_sdk::ruma::OwnedUserId;
use matrix_sdk::Room;

use tokio::sync::mpsc;

use mlua::Lua;
use askama::Template;

/// An event object passed to modules.
///
/// For modules consuming text-like events, this should contain everything that's needed.
#[derive(Clone)]
pub struct ConsumerEvent {
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
}

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
    /// Run this module if we matched a non-restricted keyword, but no exclusive module matched.
    CommandNotFound,
    /// module doesn't want this event
    Reject,
    /// module wants this event, but in a passive way; shouldn't be later rejected with ACLs (noisy)
    Inclusive,
    /// module wants this event exclusively, but doesn't mind if passthrough modules catch it as well
    Passthrough,
    /// module wants this event exclusively. mostly for keyworded commands
    Exclusive,
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

#[derive(Template, Clone)]
#[template(
    path = "matrix/help-list.html",
    blocks = ["formatted", "plain", "wiki"],
)]
pub(crate) struct RenderList {
    pub(crate) modules: Vec<WeakModuleInfo>,
    pub(crate) passthrough: Vec<WeakModuleInfo>,
    pub(crate) workers: Vec<WorkerInfo>,
    pub(crate) config: Option<Config>,
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

    fn sorted_modules(&self) -> Vec<WeakModuleInfo> {
        let mut rmod = self.modules.clone();
        rmod.retain(|m| !m.name.contains("/"));
        rmod.sort_by_key(|e| e.name.clone());
        rmod
    }

    fn sorted_mun_modules(&self) -> Vec<WeakModuleInfo> {
        let mut rmod = self.modules.clone();
        rmod.retain(|m| m.name.contains("/"));
        rmod.sort_by_key(|e| e.name.clone());
        rmod
    }

    fn sorted_passthrough(&self) -> Vec<WeakModuleInfo> {
        let mut rmod = self.passthrough.clone();
        rmod.sort_by_key(|e| e.name.clone());
        rmod
    }

    fn sorted_workers(&self) -> Vec<WorkerInfo> {
        let mut rmod = self.workers.clone();
        rmod.sort_by_key(|e| e.name.clone());
        rmod
    }
}


#[derive(Template)]
#[template(
    path = "matrix/help-generic.html",
    blocks = ["formatted", "plain"],
)]
pub(crate) struct RenderHelp {
    pub(crate) config: Config,
    pub(crate) modules: Vec<WeakModuleInfo>,
    pub(crate) passthrough: Vec<WeakModuleInfo>,
    pub(crate) workers: Vec<WorkerInfo>,
    pub(crate) source_url: String,
    pub(crate) docs_link: String,
    pub(crate) matrix_contact: String,
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
