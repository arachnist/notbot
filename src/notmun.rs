use crate::{
    Config, ModuleStarter, MODULE_STARTERS,
};

use tracing::{error, trace};

use matrix_sdk::{
    event_handler::EventHandlerHandle,
    ruma::events::room::message::{
        MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
    },
    Client, Room,
};

use lazy_static::lazy_static;
use linkme::distributed_slice;
use serde_derive::Deserialize;

use mlua::{
    chunk, FromLua, Function, Lua, MetaMethod, Result, UserData, UserDataMethods, Value, Variadic, Table,
};

lazy_static! {
    static ref LUA: Lua = Lua::new();
    static ref lua_globals: Table = LUA.globals();
}

#[distributed_slice(MODULE_STARTERS)]
static MODULE_STARTER: ModuleStarter = (module_path!(), module_starter);

fn module_starter(client: &Client, config: &Config) -> anyhow::Result<EventHandlerHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;

    let _ = LUA.load(chunk!(
        irc = {}
        irc.Channel = {}
        irc.Channel.__index = irc.Channel
    ));

    Ok(client
        .add_event_handler(move |ev, client, room| lua_dispatcher(ev, client, room, module_config)))
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct ModuleConfig {
    mun_plugins_path: String,
}
/*
#[distributed_slice(WORKERS)]
static WORKER_STARTER: WorkerStarter = (module_path!(), worker_starter);

fn worker_starter(client: &Client, config: &Config) -> anyhow::Result<AbortHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
    let worker = tokio::task::spawn(presence_observer(client.clone(), module_config));
    Ok(worker.abort_handle())
}

*/

async fn lua_dispatcher(
    ev: OriginalSyncRoomMessageEvent,
    client: Client,
    _room: Room,
    _module_config: ModuleConfig,
) -> anyhow::Result<()> {
    if client.user_id().unwrap() == ev.sender {
        return Ok(());
    };

    let MessageType::Text(_text) = ev.content.msgtype else {
        return Ok(());
    };

    Ok(())
}
