use crate::{maybe_get_room, Config, DBPools, ModuleStarter, MODULE_STARTERS};

use core::{error::Error as StdError, fmt};
use std::{fs, path::Path};

use tracing::{debug, error, info, trace};

use tokio::sync::mpsc::{channel, Receiver};

use matrix_sdk::{
    event_handler::{Ctx, EventHandlerHandle},
    ruma::events::room::member::{MembershipChange, RoomMemberEvent},
    ruma::events::room::message::{MessageType, RoomMessageEvent, RoomMessageEventContent},
    ruma::events::{AnyMessageLikeEvent, AnyStateEvent, AnySyncTimelineEvent, AnyTimelineEvent},
    Client, Room,
};

use linkme::distributed_slice;
use serde_derive::Deserialize;

use futures::pin_mut;
use tokio_postgres::{types::Type, Row};
use tokio_stream::StreamExt;

use mlua::{
    chunk, ExternalResult, Function, Lua, LuaSerdeExt, Result as LuaResult, Table, Value, Variadic,
};

#[distributed_slice(MODULE_STARTERS)]
static MODULE_STARTER: ModuleStarter = (module_path!(), module_starter);

fn module_starter(client: &Client, config: &Config) -> anyhow::Result<EventHandlerHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;

    let lua: Lua = Lua::new();
    let lua_globals: Table = lua.globals();

    initialize_lua_env(&lua, &lua_globals, &module_config)?;

    let (tx, rx) = channel::<IRCAction>(1);
    let lua_matrix: Table = lua.create_table()?;

    let proxy_tx = tx.clone();
    let proxy: Function = lua.create_async_function(move |_, msg: Variadic<String>| {
        let msg_tx = proxy_tx.clone();
        async move {
            let action: IRCAction = msg.to_vec().try_into().into_lua_err()?;

            if let Err(e) = msg_tx.send(action).await {
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

    debug!("attempting to match event");
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
        AnyTimelineEvent::State(AnyStateEvent::RoomMember(RoomMemberEvent::Original(event))) => {
            info!(
                "membership change: {target} {:#?} {}",
                event.membership_change(),
                event.state_key
            );

            match event.membership_change() {
                MembershipChange::Invited => {
                    trace!("membershi content: {event:#?}");
                    // needed to properly fill-up channel objects
                    if event.state_key != client.user_id().unwrap() {
                        debug!(
                            "event not for us: {}, {}",
                            event.state_key,
                            client.user_id().unwrap()
                        );
                        return Ok(());
                    };
                    debug!("calling irc:Join for {target}");
                    lua.load(chunk! {
                        irc:Join($target)
                    })
                    .exec()?;

                    Ok(())
                }
                _ => Ok(()),
            }
        }
        _ => Ok(()),
    }
}

async fn consumer(client: Client, mut rx: Receiver<IRCAction>) -> anyhow::Result<()> {
    loop {
        let action = match rx.recv().await {
            Some(m) => m,
            None => continue,
        };

        let room = action.get_room(&client).await?;

        match action {
            IRCAction::Say(_, _) | IRCAction::Notice(_, _) | IRCAction::Html(_, _) => {
                room.send(action.get_message()?).await?;
            }
            e => {
                error!("{}", NotMunError::UnhandledAction(e));
            }
        }
    }
}

fn initialize_lua_env(lua: &Lua, global: &Table, config: &ModuleConfig) -> anyhow::Result<()> {
    global.set("MUN_PATH", config.mun_path.clone())?;

    global.set(
        "async_fetch_http",
        lua.create_async_function(|lua, uri| async move {
            async_fetch_http(lua, uri).await.into_lua_err()
        })?,
    )?;
    global.set(
        "fetch_json",
        lua.create_async_function(|lua, uri: String| async move {
            let resp = reqwest::get(&uri)
                .await
                .and_then(|resp| resp.error_for_status())
                .into_lua_err()?;
            let json = resp.json::<serde_json::Value>().await.into_lua_err()?;
            lua.to_value(&json)
        })?,
    )?;
    global.set(
        "r_debug",
        lua.create_function(|_, value: Value| {
            debug!("{value:#?}");
            Ok(())
        })?,
    )?;
    global.set(
        "rust_db_query_wrapper",
        lua.create_async_function(
            |lua, (handle, statement, query_args): (String, String, Variadic<String>)| async move {
                debug!(
                    "in db query wrapper: {handle} | {statement} | {:#?}",
                    query_args
                );
                let res = lua_db_query(lua, &handle, &statement, query_args)
                    .await
                    .into_lua_err()?;
                trace!("returning from query wrapper: {:#?}", res);
                LuaResult::Ok(res)
            },
        )?,
    )?;

    Ok(())
}

async fn lua_db_query(
    lua: Lua,
    handle: &str,
    statement_str: &str,
    query_args: Variadic<String>,
) -> LuaResult<Table> {
    debug!("aquiring client for {handle}");
    let client = DBPools::get_client(handle).await.into_lua_err()?;
    debug!("preparing statement with {statement_str}");
    let statement = client.prepare(statement_str).await.into_lua_err()?;

    debug!("executing query");
    let results_stream = client
        .query_raw(&statement, query_args.to_vec())
        .await
        .into_lua_err()?;
    debug!("query executed");

    pin_mut!(results_stream);

    debug!("constructing response");
    let lua_result = lua.create_table()?;

    while let Some(result) = results_stream.next().await {
        let row: Row = match result {
            Ok(r) => r,
            Err(_) => break,
        };
        trace!("returned row: {:#?}", row);

        let lua_row: Table = lua_db_row_to_table(&lua, &row)?;

        lua_result.push(lua_row)?;
    }

    trace!("returning results {:#?}", lua_result);

    LuaResult::Ok(lua_result)
}

fn lua_db_row_to_table(lua: &Lua, row: &Row) -> LuaResult<Table> {
    let lua_row: Table = lua.create_table()?;

    for (i, rcol) in row.columns().iter().enumerate() {
        debug!("column type is: {:#?}", rcol.type_());
        // lua_row.set(rcol.name(), row.get::<usize, >(i))?,
        match rcol.type_().to_owned() {
            Type::INT8 => lua_row.set(rcol.name(), row.get::<usize, i64>(i))?,
            _ => lua_row.set(rcol.name(), row.get::<usize, String>(i))?,
        }
    }

    LuaResult::Ok(lua_row)
}

async fn async_fetch_http(lua: Lua, uri: String) -> anyhow::Result<(String, u16, Table)> {
    let resp = reqwest::get(&uri)
        .await
        .and_then(|resp| resp.error_for_status())
        .into_lua_err()?;

    let code = resp.status().as_u16();
    let headers: mlua::Table = lua.create_table()?;

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

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) enum IRCAction {
    Say(String, String),
    Html(String, String),
    Notice(String, String),
    Kick(String, String, String),
    SetNick(String, String),
}

impl IRCAction {
    async fn get_room(&self, c: &Client) -> anyhow::Result<Room> {
        match self {
            IRCAction::Say(room, _)
            | IRCAction::Html(room, _)
            | IRCAction::Notice(room, _)
            | IRCAction::Kick(room, _, _)
            | IRCAction::SetNick(room, _) => Ok(maybe_get_room(c, room).await?),
        }
    }

    fn get_message(&self) -> anyhow::Result<RoomMessageEventContent> {
        match self {
            IRCAction::Say(_, message) | IRCAction::Notice(_, message) => {
                Ok(RoomMessageEventContent::text_plain(message))
            }
            IRCAction::Html(_, message) => Ok(RoomMessageEventContent::text_html(message, message)),
            _ => Err(NotMunError::UnhandledAction(self.clone()).into()),
        }
    }
}

impl fmt::Display for IRCAction {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            IRCAction::Say(room, message) => write!(fmt, "Say: {room} <- {message}"),
            IRCAction::Html(room, message) => write!(fmt, "Html: {room} <- {message}"),
            IRCAction::Notice(room, message) => write!(fmt, "Notice: {room} <- {message}"),
            IRCAction::Kick(room, target, reason) => {
                write!(fmt, "Kick: {room} -> {target}: {reason}")
            }
            IRCAction::SetNick(room, display_name) => {
                write!(fmt, "Present as: {room} -> {display_name}")
            }
        }
    }
}

impl TryFrom<Vec<String>> for IRCAction {
    type Error = NotMunError;

    fn try_from(msg: Vec<String>) -> Result<IRCAction, NotMunError> {
        let first = msg[0].clone();
        match first.as_str() {
            "Say" => {
                let room: String = msg[1].clone();
                let message: String = msg[2].clone();
                Ok(IRCAction::Say(room, message))
            }
            "Html" => {
                let room: String = msg[1].clone();
                let message: String = msg[2].clone();
                Ok(IRCAction::Html(room, message))
            }
            "Notice" => {
                let room: String = msg[1].clone();
                let message: String = msg[2].clone();
                Ok(IRCAction::Notice(room, message))
            }
            "Kick" => {
                let room: String = msg[1].clone();
                let target: String = msg[2].clone();
                let message: String = msg[3].clone();
                Ok(IRCAction::Kick(room, target, message))
            }
            "SetNick" => {
                let room: String = msg[1].clone();
                let display_name: String = msg[2].clone();
                Ok(IRCAction::SetNick(room, display_name))
            }
            &_ => Err(NotMunError::UnknownAction),
        }
    }
}

#[derive(Debug)]
pub(crate) enum NotMunError {
    NoRoom(String),
    UnknownAction,
    UnhandledAction(IRCAction),
}

impl StdError for NotMunError {}

impl fmt::Display for NotMunError {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            NotMunError::NoRoom(e) => write!(fmt, "Couldn't get room from: {e}"),
            NotMunError::UnknownAction => write!(fmt, "Unknown action"),
            NotMunError::UnhandledAction(e) => write!(fmt, "Unhandled action: {e}"),
        }
    }
}
