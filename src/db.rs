use crate::{Config, ModuleStarter, MODULE_STARTERS};

use core::{error::Error as StdError, fmt};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tracing::{error, info, trace};

use linkme::distributed_slice;
use matrix_sdk::{
    event_handler::EventHandlerHandle,
    ruma::events::room::message::OriginalSyncRoomMessageEvent,
    ruma::events::room::message::{MessageType, RoomMessageEventContent},
    Client, Room,
};
use prometheus::Counter;
use prometheus::{opts, register_counter};

use lazy_static::lazy_static;

use deadpool_postgres::{
    Client as DBClient, Config as PGConfig, ManagerConfig, Pool, RecyclingMethod, Runtime,
};
use tokio_postgres::NoTls;

lazy_static! {
    static ref DB_STATUS: Counter = register_counter!(opts!(
        "db_status_requests_total",
        "Number of DB status requests",
    ))
    .unwrap();
    static ref DB_CONNECTIONS: DBPools = Default::default();
}

pub type DBConfig = HashMap<String, PGConfig>;

#[derive(Default)]
pub struct DBPools(Arc<Mutex<HashMap<String, Pool>>>);

impl DBPools {
    pub(crate) async fn get_pool(handle: &str) -> Result<Pool, DBError> {
        trace!("aquiring lock");
        let dbc = match DB_CONNECTIONS.0.lock() {
            Ok(d) => d,
            Err(_) => return Err(DBError::CollectionLock),
        };

        trace!("aquiring pool");
        match dbc.get(handle) {
            Some(p) if !p.is_closed() => Ok(p.clone()),
            _ => Err(DBError::HandleNotFound),
        }
    }

    pub(crate) async fn get_client(handle: &str) -> Result<DBClient, DBError> {
        let pool = {
            trace!("aquiring lock");
            let dbc = match DB_CONNECTIONS.0.lock() {
                Ok(d) => d,
                Err(_) => return Err(DBError::CollectionLock),
            };

            trace!("aquiring pool");
            match dbc.get(handle) {
                Some(p) if !p.is_closed() => p.clone(),
                _ => return Err(DBError::HandleNotFound),
            }
        };

        trace!("aquiring client");
        match pool.get().await {
            Ok(client) => {
                trace!("client aquired");
                Ok(client)
            }
            Err(_) => {
                trace!("acquiring failed");
                Err(DBError::GetClient)
            }
        }
    }
}

#[distributed_slice(MODULE_STARTERS)]
static MODULE_STARTER: ModuleStarter = (module_path!(), module_starter);

fn module_starter(client: &Client, config: &Config) -> anyhow::Result<EventHandlerHandle> {
    info!("registering database connections");
    let mut module_config: DBConfig = config.module_config_value(module_path!())?.try_into()?;

    let mut dbc = DB_CONNECTIONS.0.lock().unwrap();
    for (name, pool) in dbc.iter() {
        info!("closing db conn: {name}");
        pool.close();
    }

    for (name, dbcfg) in &mut module_config {
        info!("new db conn: {name}");
        dbcfg.manager = Some(ManagerConfig {
            recycling_method: RecyclingMethod::Verified,
        });

        let pool = match dbcfg.create_pool(Some(Runtime::Tokio1), NoTls) {
            Ok(p) => p,
            Err(_) => {
                error!("couldn't create database pool for {name}");
                continue;
            }
        };

        dbc.insert(name.to_owned(), pool);
    }

    Ok(client.add_event_handler(move |ev, client, room| {
        module_entrypoint(ev, client, room, module_config)
    }))
}

async fn module_entrypoint(
    ev: OriginalSyncRoomMessageEvent,
    client: Client,
    room: Room,
    config: DBConfig,
) -> anyhow::Result<()> {
    if client.user_id().unwrap() == ev.sender {
        return Ok(());
    };

    let MessageType::Text(text) = ev.content.msgtype else {
        return Ok(());
    };

    if !text.body.trim().starts_with(".db") {
        return Ok(());
    };

    DB_STATUS.inc();

    trace!("building response");
    let response = {
        let mut wip_response: String = "database status:".to_string();

        trace!("attempting to grab dbc lock");
        // let dbc = DB_CONNECTIONS.0.lock().unwrap();
        // for (name, pool) in dbc.iter() {
        for name in config.keys() {
            trace!("checking connection: {name}");
            let dbpool = DBPools::get_pool(name).await?;

            if dbpool.is_closed() {
                wip_response.push_str(format!(" {name}: closed;").as_str());
            } else {
                wip_response.push_str(format!(" {name}: open,").as_str());

                let client = dbpool.get().await?;
                let stmt = client.prepare_cached("SELECT 'foo' || $1").await?;
                if (client.query(&stmt, &[&"bar"]).await).is_err() {
                    wip_response.push_str(" broken: {e}");
                } else {
                    wip_response.push_str(" functional");
                };
            };
        }

        trace!("returning response part: {wip_response}");
        wip_response
    };

    trace!("sending response");
    room.send(RoomMessageEventContent::text_plain(response))
        .await?;

    Ok(())
}

#[derive(Debug)]
pub enum DBError {
    CollectionLock,
    HandleNotFound,
    GetClient,
}

impl StdError for DBError {}

impl fmt::Display for DBError {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            DBError::CollectionLock => write!(fmt, "Couldn't aquire connection collection lock"),
            DBError::HandleNotFound => write!(fmt, "Handle not found in connections"),
            DBError::GetClient => write!(fmt, "Couldn't get client from pool"),
        }
    }
}
