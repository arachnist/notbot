use crate::{Config, ModuleStarter, MODULE_STARTERS};

use std::{fs, path::Path};

use tracing::{error, info};

use tokio::sync::mpsc::{channel, Receiver};

use matrix_sdk::{
    event_handler::{Ctx, EventHandlerHandle},
    ruma::events::room::message::{MessageType, RoomMessageEvent, RoomMessageEventContent},
    ruma::events::{AnyMessageLikeEvent, AnySyncTimelineEvent, AnyTimelineEvent},
    ruma::{OwnedRoomAliasId, OwnedRoomId},
    Client, Room,
};

use linkme::distributed_slice;
use serde_derive::Deserialize;

use mlua::{chunk, Function, Lua, Table};
/*
lazy_static! {
    static ref LUA: Lua = Lua::new();
    static ref lua_globals: Table = LUA.globals();
}
*/

#[derive(Debug, Clone, PartialEq, Deserialize)]
enum IRCAction {
    Say(String, String),
    Notice(String, String),
    Kick(String, String, String),
    SetNick(String, String),
}

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
    lua.load(fs::read_to_string(startmun)?)
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

    // FIXME: an absolutely horrible channel, but i couldn't get anything else to work :(
    let (tx, rx) = channel::<Vec<String>>(1);
    let lua_matrix: Table = lua.create_table()?;

    let say_tx = tx.clone();
    let say: Function =
        lua.create_async_function(move |_, (target, message): (String, String)| {
            let msg_tx = say_tx.clone();
            async move {
                if let Err(e) = msg_tx.send(vec!["Say".to_string(), target, message]).await {
                    error!("couldn't send irc message to pipe: {e}");
                };
                Ok(())
            }
        })?;

    let notice_tx = tx.clone();
    let notice: Function =
        lua.create_async_function(move |_, (target, message): (String, String)| {
            let msg_tx = notice_tx.clone();
            async move {
                if let Err(e) = msg_tx
                    .send(vec!["Notice".to_string(), target, message])
                    .await
                {
                    error!("couldn't send irc message to pipe: {e}");
                };
                Ok(())
            }
        })?;

    let kick_tx = tx.clone();
    let kick: Function =
        lua.create_async_function(move |_, (room, target, reason): (String, String, String)| {
            let msg_tx = kick_tx.clone();
            async move {
                if let Err(e) = msg_tx
                    .send(vec!["Kick".to_string(), room, target, reason])
                    .await
                {
                    error!("couldn't send irc message to pipe: {e}");
                };
                Ok(())
            }
        })?;

    let set_nick_tx = tx.clone();
    let set_nick: Function =
        lua.create_async_function(move |_, (target, message): (String, String)| {
            let msg_tx = set_nick_tx.clone();
            async move {
                if let Err(e) = msg_tx
                    .send(vec!["SetNick".to_string(), target, message])
                    .await
                {
                    error!("couldn't send irc message to pipe: {e}");
                };
                Ok(())
            }
        })?;

    let _ = lua_matrix.set("Say", say);
    let _ = lua_matrix.set("Notice", notice);
    let _ = lua_matrix.set("Kick", kick);
    let _ = lua_matrix.set("SetNick", set_nick);

    let _ = lua_globals.set("Matrix", lua_matrix);

    tokio::task::spawn(consumer(client.clone(), rx));

    client.add_event_handler_context(lua);

    Ok(client.add_event_handler(move |ev, client, room, lua| {
        lua_dispatcher(ev, client, room, lua, module_config)
    }))
}

// FIXME: goes without saying
async fn consumer(client: Client, mut rx: Receiver<Vec<String>>) -> anyhow::Result<()> {
    loop {
        let msg = match rx.recv().await {
            Some(m) => m,
            None => continue,
        };

        let maybe_room: String = msg[1].clone();

        let room_id: OwnedRoomId = match maybe_room.clone().try_into() {
            Ok(r) => r,
            Err(_) => {
                let alias_id = OwnedRoomAliasId::try_from(maybe_room)?;

                client.resolve_room_alias(&alias_id).await?.room_id
            }
        };

        let room = match client.get_room(&room_id) {
            Some(r) => r,
            None => continue,
        };

        if msg[0] == "Say" {
            room.send(RoomMessageEventContent::text_plain(msg[2].clone()))
                .await?;
            continue;
        };
        if msg[0] == "Notice" {
            room.send(RoomMessageEventContent::notice_plain(msg[2].clone()))
                .await?;
            continue;
        }
    }
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct ModuleConfig {
    mun_path: String,
    lua_path: String,
}

async fn lua_dispatcher(
    ev: AnySyncTimelineEvent,
    client: Client,
    room: Room,
    lua: Ctx<Lua>,
    _module_config: ModuleConfig,
) -> anyhow::Result<()> {
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

    match event {
        AnyTimelineEvent::MessageLike(AnyMessageLikeEvent::RoomMessage(
            RoomMessageEvent::Original(event),
        )) => {
            match &event.content.msgtype {
                MessageType::Notice(content) => {
                    handle_command.call::<()>((
                        "", // FIXME: figure out the missing argument
                        "irc.Notice",
                        event.sender.to_string(),
                        target.as_str(),
                        content.body.as_str(),
                    ))?;
                    Ok(())
                }
                MessageType::Text(content) => {
                    if let Err(e) = handle_command.call::<()>((
                        "", // FIXME: figure out the missing argument
                        "irc.Message",
                        event.sender.to_string(),
                        target.as_str(),
                        content.body.as_str(),
                    )) {
                        error!("error: {}", e);
                    }
                    Ok(())
                }
                _ => Ok(()),
            }
        }
        _ => Ok(()),
    }
}
