mod botmanager;
mod config;
mod db;
mod metrics;
mod tools;

pub use crate::botmanager::BotManager;
pub mod prelude;

mod autojoiner;
mod inviter;
mod kasownik;
mod notbottime;
mod notmun;
mod shenanigans;
mod spaceapi;
mod webterface;
mod wolfram;

use crate::prelude::*;

fn register_modules(
    mx: &Client,
    config: &Config,
    modules: &mut HashMap<String, Module>,
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

        modules.insert(name.to_string(), Module { handle, starter });
    }
}

pub(crate) fn init_modules(
    mx: &Client,
    config: &Config,
    old_modules: &HashMap<String, Module>,
) -> (HashMap<String, Module>, Vec<String>) {
    let mut modules: HashMap<String, Module> = Default::default();
    let mut failed: Vec<String> = vec![];

    for (name, module) in old_modules {
        match &module.handle {
            Some(handle) => {
                info!("deregistering: {name}");
                mx.remove_event_handler(handle.to_owned());
            }
            None => info!("module was previously not registerd: {name}"),
        };
    }

    for initializer in vec![
        autojoiner::modules,
        db::modules,
        inviter::modules,
        kasownik::modules,
        notmun::modules,
        spaceapi::modules,
        wolfram::modules,
        shenanigans::modules,
    ] {
        register_modules(mx, config, &mut modules, &mut failed, initializer());
    }

    (modules, failed)
}

use tokio::task::AbortHandle;
fn register_workers(
    mx: &Client,
    config: &Config,
    modules: &mut HashMap<String, Worker>,
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

        modules.insert(name.to_string(), Worker { handle, starter });
    }
}

pub(crate) fn init_workers(
    mx: &Client,
    config: &Config,
    old_workers: &HashMap<String, Worker>,
) -> (HashMap<String, Worker>, Vec<String>) {
    let mut workers: HashMap<String, Worker> = Default::default();
    let mut failed: Vec<String> = vec![];

    for (name, worker) in old_workers {
        match &worker.handle {
            Some(handle) => {
                info!("stopping: {name}");
                handle.abort();
            }
            None => info!("worker was previously not started: {name}"),
        };
    }

    for initializer in vec![
        spaceapi::workers,
        webterface::workers,
    ] {
        register_workers(mx, config, &mut workers, &mut failed, initializer());
    }

    (workers, failed)
}
