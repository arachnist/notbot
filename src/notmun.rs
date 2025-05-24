//! Run a modified IRC bot inside your Matrix bot for fun and profit.
//!
//! Builds a Lua environment, and provides some helper/proxy functions to make a slightly modified version of
//! [mun](https://code.hackerspace.pl/ar/notmun) work with not too many changes compared to the original.
//!
//! # Configuration
//!
//! ```toml
//! [module."notbot::notmun"]
//! # String; required; path to notmun
//! mun_path = "../mun"
//! ```
//!
//! # Usage
//!
//! Depends on what modules are enabled in the notmun version the configuration points at.
//! Some core functions include:
//! * `plugin-reload` - reloads a notmun plugin
//! * `eval` - evaluates a bit of lua code in best-effort sandboxed environment
//! * `eval-core` - evaluates a bit of lua code in the global notmun environment

use crate::prelude::*;

use futures::pin_mut;
use tokio_postgres::{Row, types::Type};
use tokio_stream::StreamExt;

use mlua::{
    ExternalError, ExternalResult, Lua, LuaSerdeExt, Result as LuaResult, Table, Value, Variadic,
    chunk,
};

#[allow(clippy::cognitive_complexity, reason = "false positive: just an iteration over directory listing + appending two vectors")]
pub(crate) fn module_starter(
    client: &Client,
    config: &Config,
) -> anyhow::Result<(Vec<ModuleInfo>, Vec<PassThroughModuleInfo>)> {
    let lua: Lua = Lua::new();

    let mut modules: Vec<ModuleInfo> = vec![];
    let mut passthrough: Vec<PassThroughModuleInfo> = vec![];

    let plugins_path = format!("{mun_path}/plugins/", mun_path = config.mun_path(),);

    for entry in fs::read_dir(plugins_path)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "lua") {
                let Some(fname) = path.file_name() else {
                    error!(
                        "wtf? tried to read a file name of a file that we already checked extension for, and failed: {:?}",
                        path
                    );
                    continue;
                };

                let Some(fname_str) = fname.to_str() else {
                    error!("wtf? apparently not valid unicode: {:?}", fname);
                    continue;
                };

                let Some(plugin_id) = fname_str.strip_suffix(".lua") else {
                    error!(
                        "wtf? stripping extension from a file name that we already checked extension for failed: {:?}",
                        fname
                    );
                    continue;
                };

                let (rmod, rpass) = match mun_load_plugin(&lua, config, plugin_id) {
                    Ok(r) => r,
                    Err(e) => {
                        error!("loading plugin {plugin_id} failed: {e}");
                        continue;
                    }
                };

                modules.extend(rmod);
                passthrough.extend(rpass);
            }
        }
    }

    client.add_event_handler_context(lua);

    Ok((modules, passthrough))
}

/// Completes setting up plugin environment, and loads a Mun plugin.
///
/// # Errors
/// Will return `Err` if manipulating lua tables fails, or locking of constructed modules list fails.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity, reason = "splitting this up wouldn't make sense")]
pub fn mun_load_plugin(
    lua: &Lua,
    config: &Config,
    plugin_id: &str,
) -> anyhow::Result<(Vec<ModuleInfo>, Vec<PassThroughModuleInfo>)> {
    trace!("{plugin_id}: loading");
    let full_plugin_env = &mun_plugin_env(lua)?;
    let modules: Arc<Mutex<Vec<ModuleInfo>>> = Arc::new(Mutex::new(vec![]));
    let mut retmodules: Vec<ModuleInfo> = vec![];
    let passthrough: Arc<Mutex<Vec<PassThroughModuleInfo>>> = Arc::new(Mutex::new(vec![]));
    let mut retpassthrough: Vec<PassThroughModuleInfo> = vec![];

    let print_unbound = lua.create_function(move |_, (plugin_id, message): (String, String)| {
        info!("[{plugin_id}]: {message}");
        LuaResult::Ok(())
    })?;
    let error_unbound = lua.create_function(move |_, (plugin_id, message): (String, String)| {
        error!("[{plugin_id}]: {message}");
        LuaResult::Ok(())
    })?;
    full_plugin_env.set("print", print_unbound.bind(plugin_id)?)?;
    full_plugin_env.set("error", error_unbound.bind(plugin_id)?)?;

    let plugin_env: &Table = &full_plugin_env.get("plugin")?;

    trace!("{plugin_id}: initializing ConfigGet");
    let config_get_unbound = lua.create_function({
        let config = config.clone();
        move |lua, (plugin, key): (String, String)| {
            let config_section = format!("mun_plugin_{plugin}");
            let plugin_config: HashMap<String, toml::Value> = config
                .clone()
                .typed_module_config(&config_section)
                .into_lua_err()?;

            match plugin_config.get(&key) {
                Some(v) => LuaResult::Ok(Some(lua.to_value(v)?)),
                None => LuaResult::Ok(None),
            }
        }
    })?;
    let config_get = config_get_unbound.bind(plugin_id.to_owned())?;
    plugin_env.set("ConfigGet", config_get)?;

    trace!("{plugin_id}: initializing AddCommand");
    let add_command_unbound = lua.create_function({
        let modules = modules.clone();
        move |_,
              (plugin_env, name, arity, callback, maybe_help, maybe_klacz_level): (
            Table,
            String,
            i64,
            mlua::Function,
            Option<String>,
            Option<i64>,
        )| {
            trace!("{name}: loading module");
            let Ok(mut modules) = modules.lock() else {
                return Err(mlua::Error::runtime(format!(
                    "{name}: locking modules failed"
                )));
            };

            if !callback.set_environment(plugin_env)? {
                error!("{name}: setting sandbox env for failed");
                return Err(mlua::Error::runtime(format!(
                    "{name}: setting sandbox env for failed"
                )));
            };

            modules.push(ModuleInfo::new_mun_command(
                &name,
                arity,
                callback,
                maybe_help,
                maybe_klacz_level,
            ));
            drop(modules);
            info!("{name} module loaded");

            LuaResult::Ok(())
        }
    })?;
    let add_command = add_command_unbound.bind(full_plugin_env)?;
    plugin_env.set("AddCommand", add_command)?;

    trace!("{plugin_id}: initializing AddHook");
    let add_hook_unbound = lua.create_function({
        let passthrough = passthrough.clone();
        #[allow(clippy::cognitive_complexity, reason = "false positive")]
        move |_,
                  (plugin_env, event_name, name, callback): (
                Table,
                String,
                String,
                mlua::Function,
            )| {
                trace!("{name}: loading hook");
                let Ok(mut passthrough) = passthrough.lock() else {
                    error!("{name}: locking passthrough failed");
                    return Err(mlua::Error::runtime(format!(
                        "{name}: locking passthrough failed"
                    )));
                };

                if !callback.set_environment(plugin_env)? {
                    error!("{name}: setting sandbox env for failed");
                    return Err(mlua::Error::runtime(format!(
                        "{name}: setting sandbox env for failed"
                    )));
                };

                passthrough.push(PassThroughModuleInfo::new_mun_hook(
                    &event_name,
                    &name,
                    callback,
                ));
                drop(passthrough);
                info!("{name} hook loaded");

                LuaResult::Ok(())
            }
    })?;
    let add_hook = add_hook_unbound.bind(full_plugin_env)?;
    plugin_env.set("AddHook", add_hook)?;

    full_plugin_env.set("plugin", plugin_env)?;

    let plugin_path = format!(
        "{mun_path}/plugins/{plugin_id}.lua",
        mun_path = config.mun_path(),
    );

    info!("loading Mun plugin {plugin_id} from {plugin_path}");
    lua.load(fs::read_to_string(plugin_path)?)
        .set_name(plugin_id)
        .set_environment(full_plugin_env.to_owned())
        .set_mode(mlua::ChunkMode::Text)
        .exec()?;

    let Ok(locked_modules) = modules.lock() else {
        bail!("locking modules failed")
    };
    for module in locked_modules.iter() {
        retmodules.push(module.clone());
    }
    drop(locked_modules);

    let Ok(locked_passthrough) = passthrough.lock() else {
        bail!("locking passthrough failed");
    };
    for module in locked_passthrough.iter() {
        retpassthrough.push(module.clone());
    }
    drop(locked_passthrough);

    Ok((retmodules, retpassthrough))
}

/// Prepares environment (lua "upvalue") for Mun plugins.
///
/// Mostly an analogue of `core.plugin.PrepareEnvironment` in Mun, with the following differences:
/// * doesn't perform per-plugin bindings (this is done at plugin load time
///
/// # Errors
/// Will return `Err` if mlua calls to manipulate the prepared env table fail.
#[allow(clippy::cognitive_complexity, reason = "false positive: just a bunch of variable gets/sets")]
pub fn mun_plugin_env(lua: &Lua) -> anyhow::Result<mlua::Table> {
    trace!("plugin env: initializing");
    let env_table = lua.create_table()?;
    let globals = lua.globals();

    trace!("plugin env: require(string) ");
    env_table.set(
        "string",
        lua.load(chunk! { return require("string") })
            .call::<mlua::Table>(())?,
    )?;
    trace!("plugin env: require(table) ");
    env_table.set(
        "table",
        lua.load(chunk! { return require("table") })
            .call::<mlua::Table>(())?,
    )?;
    trace!("plugin env: require(math) ");
    env_table.set(
        "math",
        lua.load(chunk! { return require("math") })
            .call::<mlua::Table>(())?,
    )?;

    trace!("plugin env: initializing http client");
    let http = &lua.create_table()?;
    http.set(
        "request",
        lua.create_async_function(|lua, uri| async move {
            async_fetch_http(lua, uri).await.into_lua_err()
        })?,
    )?;
    env_table.set("http", http)?;
    env_table.set("https", http)?;

    trace!("plugin env: initializing json decoder");
    // why is this nested like this? idk. the lua json library originally used in Mun did this
    let json = lua.create_table()?;
    let json_decode_table = lua.create_table()?;
    let json_decode = lua.create_async_function(async move |lua, payload: String| {
        let json_val: Result<serde_json::Value, serde_json::Error> = serde_json::from_str(&payload);

        match json_val {
            Ok(v) => Ok(lua.to_value(&v)?),
            Err(e) => LuaResult::Err(e.into_lua_err()),
        }
    })?;
    json_decode_table.set("decode", json_decode)?;
    json.set("decode", json_decode_table)?;
    env_table.set("json", json)?;

    trace!("plugin env: initializing misc sandbox functions");
    for funcname in ["pairs", "tonumber", "tostring", "pcall"] {
        let func: mlua::Function = globals.get(funcname)?;
        env_table.set(funcname, func)?;
    }

    let os = &lua.create_table()?;
    let g_os: Table = globals.get("os")?;
    let time: mlua::Function = g_os.get("time")?;
    os.set("time", time)?;
    env_table.set("os", os)?;

    let plugin = lua.create_table()?;

    // mun.core.plugin.API.DBOpen
    let db_open = lua.create_async_function(|lua, handle: String| async move {
        let conn = lua.create_table()?;

        let query_unbound = lua.create_async_function(
            async move |lua,
                        (handle, _, statement, query_args): (
                String,
                Table,
                String,
                Variadic<String>,
            )| {
                let iter = lua_db_query(&lua, &handle, &statement, query_args)
                    .await
                    .into_lua_err()?;

                trace!("got iterator from query");

                LuaResult::Ok(iter)
            },
        )?;
        let query = query_unbound.bind(handle)?;
        conn.set("Query", query)?;

        LuaResult::Ok(conn)
    })?;
    plugin.set("DBOpen", db_open)?;

    // mun.core.plugin.API.CurrentTime
    let time: mlua::Function = g_os.get("time")?;
    plugin.set("CurrentTime", time)?;

    // values missing from env_table.plugin at this point vs Mun:
    // * `Register` - unused
    // * `Sleep` - unused
    // Need to be set per-plugin:
    // * `ConfigGet`, used only for paczkomate and ppsa, which require (also missing) redis
    // * `AddCommand`
    // * `AddHook`

    env_table.set("plugin", plugin)?;
    env_table.set("_G", &env_table)?;

    env_table.set(
        "r_trace",
        lua.create_function(|_, value: Variadic<Value>| {
            trace!("[mun]: {value:#?}");
            Ok(())
        })?,
    )?;
    env_table.set(
        "r_format",
        lua.create_function(|_, value: Value| Ok(format!("{value:#?}")))?,
    )?;

    // values missing from env_table vs Mun:
    // * `setfenv` - only used for repl, will be replaced with [`mlua::Function::set_environment`]
    // * `redis` - not (yet?) implemented
    Ok(env_table)
}

async fn lua_db_query(
    lua: &Lua,
    handle: &str,
    statement_str: &str,
    query_args: Variadic<String>,
) -> LuaResult<mlua::Function> {
    trace!("acquiring client for {handle}");
    let client = DBPools::get_client(handle).await.into_lua_err()?;
    trace!("preparing statement with {statement_str}");
    let statement = client.prepare(statement_str).await.into_lua_err()?;

    trace!("executing query");
    let results_stream = client
        .query_raw(&statement, query_args.to_vec())
        .await
        .into_lua_err()?;
    trace!("query executed");

    pin_mut!(results_stream);

    trace!("constructing response");

    let lua_result = lua.create_table()?;

    while let Some(result) = results_stream.next().await {
        let row: Row = match result {
            Ok(r) => r,
            Err(_) => break,
        };

        let lua_row: Table = lua_db_row_to_table(lua, &row)?;

        lua_result.push(lua_row)?;
    }

    trace!("#results: {}", lua_result.len()?);

    trace!("constructing iterator");
    let iter_u = lua.create_function(|_, t:Table| {
        LuaResult::Ok(t.pop::<Table>().ok())
    })?;
    let iter = iter_u.bind(&lua_result)?;

    LuaResult::Ok(iter)
}

fn lua_db_row_to_table(lua: &Lua, row: &Row) -> LuaResult<Table> {
    let lua_row: Table = lua.create_table()?;

    for (i, rcol) in row.columns().iter().enumerate() {
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
        .and_then(reqwest::Response::error_for_status)
        .into_lua_err()?;

    let code = resp.status().as_u16();
    let headers: mlua::Table = lua.create_table()?;

    for (k, raw_v) in resp.headers() {
        headers.set(k.as_str(), raw_v.to_str().into_lua_err()?)?;
    }

    let body = (resp.text().await).unwrap_or_default();

    let rval = (body, code, headers);
    Ok(rval)
}
