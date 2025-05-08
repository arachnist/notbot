mod botmanager;
mod metrics;
mod webterface;
mod alerts;

pub use crate::botmanager::BotManager;
pub mod config;
pub mod db;
pub mod klaczdb;
pub mod module;
pub mod prelude;
pub mod tools;

mod autojoiner;
mod inviter;
mod kasownik;
mod notbottime;
mod notmun;
mod sage;
mod spaceapi;
pub mod wolfram;

use crate::prelude::*;

#[allow(deprecated)]
fn register_modules(
    mx: &Client,
    config: &Config,
    modules: &mut HashMap<String, Option<EventHandlerHandle>>,
    failed: &mut Vec<String>,
    starters: Vec<ModuleStarter>,
) {
    for (name, starter) in starters {
        info!("registering: {name}");

        let handle: Option<EventHandlerHandle> = match starter(mx, config) {
            Ok(h) => Some(h),
            Err(e) => {
                error!("initializing module {name} failed: {e}");
                failed.push(name.to_owned());
                None
            }
        };

        modules.insert(name.to_string(), handle);
    }
}

pub(crate) fn init_modules(
    mx: &Client,
    config: &Config,
    old_modules: &HashMap<String, Option<EventHandlerHandle>>,
) -> (HashMap<String, Option<EventHandlerHandle>>, Vec<String>) {
    let mut modules: HashMap<String, Option<EventHandlerHandle>> = Default::default();
    let mut failed: Vec<String> = vec![];

    for (name, module) in old_modules {
        match &module {
            Some(handle) => {
                info!("deregistering: {name}");
                mx.remove_event_handler(handle.to_owned());
            }
            None => info!("module was previously not registerd: {name}"),
        };
    }

    for initializer in [
        autojoiner::modules, // handle invites
        notmun::modules,     // just the non-text events
    ] {
        register_modules(mx, config, &mut modules, &mut failed, initializer());
    }

    (modules, failed)
}

use tokio::task::AbortHandle;
#[allow(deprecated)]
fn register_workers(
    mx: &Client,
    config: &Config,
    modules: &mut HashMap<String, Option<AbortHandle>>,
    failed: &mut Vec<String>,
    starters: Vec<WorkerStarter>,
) {
    for (name, starter) in starters {
        info!("registering: {name}");

        let handle: Option<AbortHandle> = match starter(mx, config) {
            Ok(h) => Some(h),
            Err(e) => {
                error!("initializing worker {name} failed: {e}");
                failed.push(name.to_owned());
                None
            }
        };

        modules.insert(name.to_string(), handle);
    }
}

pub(crate) fn init_workers(
    mx: &Client,
    config: &Config,
    old_workers: &HashMap<String, Option<AbortHandle>>,
) -> (HashMap<String, Option<AbortHandle>>, Vec<String>) {
    let mut workers: HashMap<String, Option<AbortHandle>> = Default::default();
    let mut failed: Vec<String> = vec![];

    for (name, worker) in old_workers {
        match &worker {
            Some(handle) => {
                info!("stopping: {name}");
                handle.abort();
            }
            None => info!("worker was previously not started: {name}"),
        };
    }

    for initializer in [spaceapi::workers, webterface::workers] {
        register_workers(mx, config, &mut workers, &mut failed, initializer());
    }

    (workers, failed)
}
