use crate::{Config, ModuleStarter, MODULE_STARTERS};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tracing::{debug, error, info, trace, warn};

use linkme::distributed_slice;
use matrix_sdk::{
    event_handler::EventHandlerHandle, ruma::events::room::message::OriginalSyncRoomMessageEvent,
    Client, Room,
};

use lazy_static::lazy_static;

use deadpool_postgres::{Config as PGConfig, ManagerConfig, Pool, RecyclingMethod, Runtime};
use tokio_postgres::NoTls;

lazy_static! {
    static ref DB_CONNECTIONS: DBPools = Default::default();
}

pub type DBConfig = HashMap<String, PGConfig>;
pub type DBPools = Arc<Mutex<HashMap<String, Pool>>>;

#[distributed_slice(MODULE_STARTERS)]
static MODULE_STARTER: ModuleStarter = (module_path!(), module_starter);

fn module_starter(client: &Client, config: &Config) -> anyhow::Result<EventHandlerHandle> {
    info!("registering database connections");
    let mut module_config: DBConfig = config.module_config_value(module_path!())?.try_into()?;
    let mut dbc = DB_CONNECTIONS.lock().unwrap();

    // let tlsconn = MakeTlsConnect::

    for (name, pool) in dbc.iter() {
        info!("closing db conn: {name}");
        pool.close();
    }

    for (name, dbcfg) in &mut module_config {
        info!("new db conn: {name}");
        dbcfg.manager = Some(ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
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

    Ok(client.add_event_handler(move |ev, room| module_entrypoint(ev, room, module_config)))
}

async fn module_entrypoint(
    _ev: OriginalSyncRoomMessageEvent,
    _room: Room,
    _module_config: DBConfig,
) -> anyhow::Result<()> {
    Ok(())
}
