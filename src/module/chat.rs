//! Chat interface to control the bot.

use super::modules::{ModuleInfo, PassThroughModuleInfo};
use super::types::{Acl, ConsumerEvent, RenderHelp, RenderList, TriggerType, WeakModuleInfo};
use super::workers::WorkerInfo;

use crate::config::Config;
use crate::tools::ToStringExt;

use anyhow::bail;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use matrix_sdk::Room;
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;

use askama::Template;

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

    let (help_wiki_tx, help_wiki_rx) = mpsc::channel::<ConsumerEvent>(1);
    let help_wiki = ModuleInfo {
        name: "help-wiki".s(),
        help: "render bot documentation page in dokuwiki format. you're likely reading this now"
            .s(),
        acl: vec![],
        trigger: TriggerType::Keyword(vec!["help-wiki".s()]),
        channel: help_wiki_tx,
        error_prefix: None,
    };
    modules.push(help_wiki);

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
    tokio::task::spawn(help_wiki_consumer(
        help_wiki_rx,
        weak_modules.clone(),
        weak_passthrough.clone(),
        registered_workers.clone(),
        config.clone(),
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
            config: None,
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

/// Provides a list of all registered modules, passthrough modules, and workers in dokuwiki format
///
/// # Errors
/// Will return `Err` when its own channel gets dropped, or rendering response fails.
pub async fn help_wiki_consumer(
    mut rx: mpsc::Receiver<ConsumerEvent>,
    modules: Vec<WeakModuleInfo>,
    passthrough: Vec<WeakModuleInfo>,
    workers: Vec<WorkerInfo>,
    config: Config,
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
            config: Some(config.clone()),
        };

        let response = RoomMessageEventContent::text_html(
            render_list.as_wiki().render()?,
            render_list.as_wiki().render()?,
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
