use crate::{Config, ModuleStarter, MODULE_STARTERS};

use std::{fs, path::Path};

use tracing::{debug, error, info, trace, warn};

use matrix_sdk::{
    event_handler::{Ctx, EventHandlerHandle},
    ruma::events::room::message::{
        MessageType, OriginalSyncRoomMessageEvent, RoomMessageEvent, RoomMessageEventContent,
    },
    ruma::events::{AnyMessageLikeEvent, AnyStateEvent, AnySyncTimelineEvent, AnyTimelineEvent},
    Client, Room,
};

use lazy_static::lazy_static;
use linkme::distributed_slice;
use serde_derive::Deserialize;

use mlua::{
    chunk, FromLua, Function, Lua, MetaMethod, Result, Table, UserData, UserDataMethods, Value,
    Variadic,
};
/*
lazy_static! {
    static ref LUA: Lua = Lua::new();
    static ref lua_globals: Table = LUA.globals();
}
*/

#[distributed_slice(MODULE_STARTERS)]
static MODULE_STARTER: ModuleStarter = (module_path!(), module_starter);

fn module_starter(client: &Client, config: &Config) -> anyhow::Result<EventHandlerHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;

    let lua: Lua = Lua::new();
    let lua_globals: Table = lua.globals();

    let _ = lua_globals.set("MUN_PATH", module_config.mun_path.clone());
    // mlua appears to ignore LUA_PATH global, and do something silly with relative path requires
    //
    // std::env::set_var("LUA_PATH", module_config.lua_path.clone());

    /* let mun_path = module_config.mun_path.clone();
    info!("mun_path: {mun_path}");

    let _ = LUA.load(chunk! {
        local _require = require
        require = function(path)
           return lua_require($mun_path .. "/" .. path)
        end
    }).set_name("preamble").exec();*/

    let startmun = Path::new(&module_config.mun_path).join("start.lua");
    info!("startmun: {}", startmun.display());
    // info!("globals.LUA_PATH: {}", lua_globals.get::<String>("LUA_PATH")?);
    let _ = lua
        .load(fs::read_to_string(startmun)?)
        .set_name("mun start.lua")
        .exec()?;

    for room in client.joined_rooms() {
        let room_name = match room.canonical_alias() {
            Some(a) => a.to_string(),
            None => room.room_id().to_string(),
        };
        lua.load(chunk! {
            irc:Join($room_name)
        })
        .exec()?
    }

    client.add_event_handler_context(lua);

    Ok(client.add_event_handler(move |ev, client, room, lua| {
        lua_dispatcher(ev, client, room, lua, module_config)
    }))
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct ModuleConfig {
    mun_path: String,
    lua_path: String,
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
    ev: AnySyncTimelineEvent,
    client: Client,
    room: Room,
    lua: Ctx<Lua>,
    _module_config: ModuleConfig,
) -> anyhow::Result<()> {
    trace!("in lua dispatcher");
    let event = ev.into_full_event(room.room_id().into());

    let target = match room.canonical_alias() {
        Some(a) => a.to_string(),
        None => room.room_id().to_string(),
    };

    if client.user_id().unwrap() == event.sender() {
        return Ok(());
    };
    let lua_globals: Table = lua.globals();

    let irc: Table = lua_globals.get("irc")?;
    let handle_command: Function = irc.get("HandleCommand")?;

    trace!("attempting to match event type");
    match event {
        AnyTimelineEvent::MessageLike(event) => match event {
            AnyMessageLikeEvent::RoomMessage(event) => match event {
                RoomMessageEvent::Original(event) => match &event.content.msgtype {
                    MessageType::Notice(content) => {
                        info!("triggering on notice");
                        handle_command.call::<()>((
                            "",
                            "irc.Notice",
                            event.sender.to_string(),
                            target.as_str(),
                            content.body.as_str(),
                        ))?;
                        return Ok(());
                    }
                    MessageType::Text(content) => {
                        info!("triggering on text");
                        match handle_command.call::<()>((
                            "",
                            "irc.Message",
                            event.sender.to_string(),
                            target.as_str(),
                            content.body.as_str(),
                        )) {
                            Ok(_) => info!("ok result"),
                            Err(e) => error!("error: {}", e),
                        }
                        return Ok(());
                    }
                    _ => return Ok(()),
                },
                _ => return Ok(()),
            },
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };
}
