//! Bot module structure definitions.

use super::types::{Acl, CatchallDecider, ConsumerEvent, Consumption, TriggerType};

use crate::tools::{ToStringExt, room_name};

use std::sync::LazyLock;

use anyhow::bail;
use mlua::{ExternalResult, Lua};
use tokio::sync::mpsc;
use tracing::{error, warn};

use matrix_sdk::Room;
use matrix_sdk::ruma::events::room::message::{MessageType, RoomMessageEventContent};

use prometheus::{IntGaugeVec, opts, register_int_gauge_vec};

/// Number of Mun message receivers that are alive. Should be 0 most of the time.
pub static MUN_RECEIVERS_LIVE: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!(
        opts!(
            "mun_receivers_live",
            "Number of Mun message receivers that are alive"
        ),
        &["module"]
    )
    .unwrap()
});

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

        tokio::task::spawn(Self::consumer(
            rx,
            config,
            owned_error_prefix.clone(),
            processor,
            name.to_owned(),
        ));

        Self {
            name: name.to_owned(),
            help: help.to_owned(),
            acl,
            trigger,
            channel: tx,
            error_prefix: owned_error_prefix,
        }
    }

    /// Generic event consumer.
    ///
    /// Consumes events from the [`ConsumerEvent`] channel and passes them on to the
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

    /// Registers a Mun command as notbot module.
    #[must_use]
    pub fn new_mun_command(
        name: &str,
        keyword: &str,
        arity: i64,
        processor: mlua::Function, // Callback
        help: Option<String>,
        maybe_klacz_level: Option<i64>,
    ) -> Self {
        let (tx, rx) = mpsc::channel(1);

        tokio::task::spawn(Self::mun_command_consumer(
            rx,
            name.to_owned(),
            processor,
            arity,
        ));

        let acl = maybe_klacz_level.map_or_else(std::vec::Vec::new, |i| vec![Acl::KlaczLevel(i)]);

        Self {
            name: name.to_owned(),
            help: help.unwrap_or_else(|| format!("command {name} has no help")),
            acl,
            trigger: TriggerType::Keyword(vec![keyword.to_owned()]),
            channel: tx,
            error_prefix: None,
        }
    }

    /// Mun command event consumer.
    ///
    /// Like the generic consumer, consumes events from a [`ConsumerEvent`] channel,
    /// but constructs from them arguments that Mun commands expect.
    ///
    /// # Errors
    /// This function will return `Err` if:
    /// * the event channel is closed
    pub async fn mun_command_consumer(
        mut rx: mpsc::Receiver<ConsumerEvent>,
        name: String,
        processor: mlua::Function,
        arity: i64,
    ) -> anyhow::Result<()> {
        loop {
            let Some(event) = rx.recv().await else {
                warn!("{name} channel closed");
                bail!("channel closed");
            };

            if let Err(e) = Self::mun_preprocessor(&event, &name, &processor, arity).await {
                error!("[{name}]: {e}");
                if let Some(first) = e.to_string().lines().next() {
                    if let Err(ee) = event
                        .room
                        .send(RoomMessageEventContent::text_plain(format!(
                            "[{name}]: {first}"
                        )))
                        .await
                    {
                        error!("[{name}]: couldn't send error: {ee}");
                    }
                }
            }
        }
    }

    async fn mun_preprocessor(
        event: &ConsumerEvent,
        name: &str,
        processor: &mlua::Function,
        arity: i64,
    ) -> anyhow::Result<()> {
        let mut lua_args: Vec<&str> = vec![];

        if let Some(argstr) = &event.args {
            if arity == -1 {
                lua_args.push(argstr);
            } else {
                let split_args = argstr.split_whitespace();

                for arg in split_args {
                    lua_args.push(arg);
                }
            }
        }

        if arity == -1 && lua_args.is_empty() {
            bail!(
                "Command '{name}' expects '{arity}' arguments, got '{}'.",
                lua_args.len()
            );
        }

        if arity != -1 && lua_args.len() != usize::try_from(arity)? {
            bail!("Please provide an argument.");
        }

        let mun_channel = Self::mun_create_channel(&event.lua, name, &event.room)?;

        processor
            .call_async::<()>((event.sender.as_str(), mun_channel, lua_args.join(" ")))
            .await
            .map_err(|le| anyhow::anyhow!("mun command error: {le}"))
    }

    fn mun_create_channel(lua: &Lua, name: &str, room: &Room) -> anyhow::Result<mlua::Table> {
        let (plain_tx, plain_rx) = mpsc::channel::<String>(1);
        let (html_tx, html_rx) = mpsc::channel::<(String, String)>(1);
        tokio::task::spawn(Self::mun_send_plain(
            name.to_string(),
            room.clone(),
            plain_rx,
        ));
        tokio::task::spawn(Self::mun_send_html(name.to_string(), room.clone(), html_rx));

        let mun_channel = lua.create_table()?;
        let say = lua.create_async_function(move |_, (_, message): (mlua::Table, String)| {
            let tx = plain_tx.clone();
            async move { tx.send(message).await.into_lua_err() }
        })?;
        let html = lua.create_async_function(
            move |_, (_, plain, html): (mlua::Table, String, String)| {
                let tx = html_tx.clone();
                async move { tx.send((plain, html)).await.into_lua_err() }
            },
        )?;

        mun_channel.set("Say", say)?;
        mun_channel.set("Html", html)?;
        mun_channel.set("Name", room_name(room))?;

        Ok(mun_channel)
    }

    /// Short-lived plain message sender for Mun module call.
    ///
    /// # Errors
    /// Will `Err` if all send channels get closed, which *should* happen as soon as consumer
    /// completes loop iteration.
    pub async fn mun_send_plain(
        name: String,
        room: Room,
        mut rx: mpsc::Receiver<String>,
    ) -> anyhow::Result<()> {
        MUN_RECEIVERS_LIVE.with_label_values(&[&name]).inc();
        loop {
            let Some(message) = rx.recv().await else {
                MUN_RECEIVERS_LIVE.with_label_values(&[&name]).dec();
                bail!("channel closed");
            };

            if let Err(e) = room
                .send(RoomMessageEventContent::text_plain(message))
                .await
            {
                error!("{name}: error sending message: {e}");
            }
        }
    }

    /// Short-lived formatted message sender for Mun module call.
    ///
    /// # Errors
    /// Will `Err` if all send channels get closed, which *should* happen as soon as consumer
    /// completes loop iteration.
    pub async fn mun_send_html(
        name: String,
        room: Room,
        mut rx: mpsc::Receiver<(String, String)>,
    ) -> anyhow::Result<()> {
        MUN_RECEIVERS_LIVE.with_label_values(&[&name]).inc();
        loop {
            let Some((plain, html)) = rx.recv().await else {
                MUN_RECEIVERS_LIVE.with_label_values(&[&name]).dec();
                bail!("channel closed");
            };

            if let Err(e) = room
                .send(RoomMessageEventContent::text_html(plain, html))
                .await
            {
                error!("{name}: error sending message: {e}");
            }
        }
    }
}

/// Thin wrapper around `ModuleInfo`
///
/// Exists because the matrix-rust-sdk can only hold one extra context object per type.
#[derive(Clone)]
pub struct PassThroughModuleInfo(pub ModuleInfo);

impl PassThroughModuleInfo {
    /// Registers a Mun hook as notbot passthrough module.
    #[must_use]
    pub fn new_mun_hook(
        event_type: &str,
        name: &str,
        processor: mlua::Function, // Callback
    ) -> Self {
        // too lazy to handle other event types for now
        let decider: CatchallDecider = match event_type {
            "irc.Message" | "irc.Notice" => |_, _, _, content, _| match &content.msgtype {
                MessageType::Notice(_) | MessageType::Text(_) => Ok(Consumption::Passthrough),
                _ => Ok(Consumption::Reject),
            },
            "bot.UnknownCommand" => |_, _, _, _, _| Ok(Consumption::CommandNotFound),
            _ => |_, _, _, _, _| Ok(Consumption::Reject),
        };

        let (tx, rx) = mpsc::channel(1);
        match event_type {
            "bot.UnknownCommand" => tokio::task::spawn(Self::mun_hook_unknown_command(
                rx,
                name.to_owned(),
                processor,
            )),
            _ => tokio::task::spawn(Self::mun_hook_consumer(rx, name.to_owned(), processor)),
        };

        Self(ModuleInfo {
            name: name.to_owned(),
            help: format!("Mun hook {name}"),
            acl: vec![],
            trigger: TriggerType::Catchall(decider),
            channel: tx,
            error_prefix: None,
        })
    }

    async fn mun_hook_unknown_command(
        mut rx: mpsc::Receiver<ConsumerEvent>,
        name: String,
        processor: mlua::Function,
    ) -> anyhow::Result<()> {
        loop {
            let Some(event) = rx.recv().await else {
                warn!("{name} channel closed");
                bail!("channel closed");
            };

            let user = event.sender.as_str();
            let command = event.keyword;
            let arguments = event.args.unwrap_or_else(|| "".s());
            let Ok(mun_channel) = ModuleInfo::mun_create_channel(&event.lua, &name, &event.room)
            else {
                error!("{name}: createing mun channel failed");
                continue;
            };

            if let Err(e) = processor
                .call_async::<()>((user, mun_channel, command, arguments))
                .await
            {
                error!("{name}: mun command failed: {e}");
            };
        }
    }

    async fn mun_hook_consumer(
        mut rx: mpsc::Receiver<ConsumerEvent>,
        name: String,
        processor: mlua::Function,
    ) -> anyhow::Result<()> {
        loop {
            let Some(event) = rx.recv().await else {
                warn!("{name} channel closed");
                bail!("channel closed");
            };

            let content = event.ev.content.body();

            let Ok(mun_channel) = ModuleInfo::mun_create_channel(&event.lua, &name, &event.room)
            else {
                error!("{name}: createing mun channel failed");
                continue;
            };

            if let Err(e) = processor
                .call_async::<()>((event.sender.as_str(), mun_channel, content))
                .await
            {
                error!("{name}: mun command failed: {e}");
            };
        }
    }
}
