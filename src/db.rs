use crate::{Config, ModuleStarter, MODULE_STARTERS};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tracing::{error, info};

use linkme::distributed_slice;
use matrix_sdk::{
    event_handler::EventHandlerHandle,
    ruma::events::room::message::OriginalSyncRoomMessageEvent,
    ruma::events::room::message::{MessageType, RoomMessageEventContent},
    Client, Room,
};
use prometheus::{opts, register_counter};
use prometheus::Counter;

use lazy_static::lazy_static;

use deadpool_postgres::{Config as PGConfig, ManagerConfig, Pool, RecyclingMethod, Runtime};
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

    Ok(client.add_event_handler(module_entrypoint))
}

async fn module_entrypoint(
    ev: OriginalSyncRoomMessageEvent,
    client: Client,
    room: Room,
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

    let response = {
        let mut wip_response: String = "database status: ".to_string();

        let dbc = DB_CONNECTIONS.lock().unwrap();
        for (name, dbpool) in dbc.iter() {
            if dbpool.is_closed() {
                wip_response.push_str(format!("{name}: closed").as_str());
            } else {
                wip_response.push_str(format!("{name}: open").as_str());
            };
        }

        wip_response
    };

    room.send(RoomMessageEventContent::text_plain(response))
        .await?;

    Ok(())
}
