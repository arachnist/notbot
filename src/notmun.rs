use crate::{Config, ModuleStarter, MODULE_STARTERS};

use std::{fs, path::Path};

use tracing::{debug, error, info};

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

use mlua::{chunk, ExternalResult, Function, Lua, LuaSerdeExt, Table, Value, Variadic};

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

    let _ = lua_globals.set(
        "blocking_fetch_http",
        lua.create_function(|lua, uri| blocking_fetch_http(lua, uri).into_lua_err())?,
    );
    let _ = lua_globals.set(
        "async_fetch_http",
        lua.create_async_function(|lua, uri| async move {
            async_fetch_http(lua, uri).await.into_lua_err()
        })?,
    );
    let _ = lua_globals.set(
        "fetch_json",
        lua.create_async_function(|lua, uri: String| async move {
            let resp = reqwest::get(&uri)
                .await
                .and_then(|resp| resp.error_for_status())
                .into_lua_err()?;
            let json = resp.json::<serde_json::Value>().await.into_lua_err()?;
            lua.to_value(&json)
        })?,
    );
    lua_globals.set(
        "r_debug",
        lua.create_function(|_, value: Value| {
            debug!("{value:#?}");
            Ok(())
        })?,
    )?;

    // FIXME: an absolutely horrible channel, but i couldn't get anything else to work :(
    let (tx, rx) = channel::<Vec<String>>(1);
    let lua_matrix: Table = lua.create_table()?;

    let proxy_tx = tx.clone();
    let proxy: Function = lua.create_async_function(move |_, strings: Variadic<String>| {
        let msg_tx = proxy_tx.clone();
        async move {
            if let Err(e) = msg_tx.send(strings.into()).await {
                error!("couldn't send irc message to pipe: {e}");
            };
            Ok(())
        }
    })?;

    let _ = lua_matrix.set("Proxy", proxy);
    let _ = lua_globals.set("Matrix", lua_matrix);

    tokio::task::spawn(consumer(client.clone(), rx));

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

    let handle_command = lua.load(chunk! {
        irc:HandleCommand(...)
    });

    match event {
        AnyTimelineEvent::MessageLike(AnyMessageLikeEvent::RoomMessage(
            RoomMessageEvent::Original(event),
        )) => {
            let (ev_type, content) = match &event.content.msgtype {
                MessageType::Notice(content) => ("irc.Notice", content.body.as_str()),
                MessageType::Text(content) => ("irc.Message", content.body.as_str()),
                _ => return Ok(()),
            };
            handle_command
                .call_async::<()>((ev_type, event.sender.as_str(), target.as_str(), content))
                .await?;
            Ok(())
        }
        _ => Ok(()),
    }
}

fn blocking_fetch_http(lua: &Lua, uri: String) -> anyhow::Result<(String, u16, Table)> {
    let resp = reqwest::blocking::get(&uri)
        .and_then(|resp| resp.error_for_status())
        .into_lua_err()?;

    let code = resp.status().as_u16();
    let headers: Table = lua.create_table()?;

    for (k, raw_v) in resp.headers() {
        headers.set(k.as_str(), raw_v.to_str().into_lua_err()?)?;
    }

    let body = match resp.text() {
        Ok(r) => r,
        Err(_) => "".to_string(),
    };

    let rval = (body, code, headers);
    Ok(rval)
}

async fn async_fetch_http(lua: Lua, uri: String) -> anyhow::Result<(String, u16, Table)> {
    let resp = reqwest::get(&uri)
        .await
        .and_then(|resp| resp.error_for_status())
        .into_lua_err()?;

    let code = resp.status().as_u16();
    let headers: Table = lua.create_table()?;

    for (k, raw_v) in resp.headers() {
        headers.set(k.as_str(), raw_v.to_str().into_lua_err()?)?;
    }

    let body = match resp.text().await {
        Ok(r) => r,
        Err(_) => "".to_string(),
    };

    let rval = (body, code, headers);
    Ok(rval)
}
