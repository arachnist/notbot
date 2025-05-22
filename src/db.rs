//! Interface with Postgres-compatible database.
//!
//! Mostly useful for writing other modules. Chat functionality in this module is limited to checking status of configured database pools
//!
//! # Configuration
//!
//! The configuration is defined as `HashMap`<name: Strin, [`deadpool_postgres::Config`]>, which is why there is no "main" configuration for the module.
//!
//! Configuration fields available in that module are passed verbatim to the database pool initialization functions.
//!
//! [`DBConfig`]
//!
//! ```toml
//! [module."notbot::db".name]
//! host = "host.example.org"
//! port = 5432
//! dbname = "database"
//! user = "user"
//! password = "password"
//! ```
//!
//! # Usage
//!
//! Keywords
//! * db - lists known database connection pools, and checks if they're functional by executing a simple query. [`dbstatus`]
//!
//! # Usage as a module developer
//!
//! After acquiring a connection through [`DBPools::get_client`], [`deadpool_postgres`] configuration should be more relevant.
//! Here's a short example
//!
//! ```rust
//! use notbot::prelude::*;
//! use notbot::db::DBPools;
//!
//! async fn get_instance_id() -> anyhow::Result<i64> {
//!     let client = DBPools::get_client("main").await?;
//!     let statement = client.prepare_cached("SELECT nextval('_instance_id')").await?;
//!     let row = client.query_one(&statement, &[]).await?;
//!     row.try_get(0)
//!         .map_err(|e: tokio_postgres::Error| anyhow!(e))
//! }
//! ```
//!
//! More life-like examples can be found in other parts of the code, especially in the [`crate::klaczdb`] and [`crate::notmun`] modules.

use crate::prelude::*;

use deadpool_postgres::{
    Client as DBClient, Config as PGConfig, ManagerConfig, Pool, RecyclingMethod, Runtime,
};
use tokio_postgres::NoTls;

static DB_CONNECTIONS: LazyLock<DBPools> = LazyLock::new(Default::default);

/// Aforementioned configuration format
pub type DBConfig = HashMap<String, PGConfig>;

/// Object for interacting with the collection of database pool
#[derive(Default)]
pub struct DBPools(Arc<Mutex<HashMap<String, Pool>>>);

impl DBPools {
    pub(crate) fn get_pool(handle: &str) -> Result<Pool, DBError> {
        trace!("acquiring lock");
        let Ok(dbc) = DB_CONNECTIONS.0.lock() else {
            return Err(DBError::CollectionLock);
        };

        trace!("acquiring pool");
        match dbc.get(handle) {
            Some(p) if !p.is_closed() => Ok(p.clone()),
            _ => Err(DBError::HandleNotFound),
        }
    }

    /// Acquire a client for a database by name.
    ///
    /// # Errors
    /// Will return `Err` if:
    /// * acquiring pool collection lock fails
    /// * requested handle is unknown
    /// * acquiring client from pool fails.
    pub async fn get_client(handle: &str) -> Result<DBClient, DBError> {
        let pool = {
            trace!("acquiring lock");
            let Ok(dbc) = DB_CONNECTIONS.0.lock() else {
                return Err(DBError::CollectionLock);
            };

            trace!("acquiring pool");
            match dbc.get(handle) {
                Some(p) if !p.is_closed() => p.clone(),
                _ => return Err(DBError::HandleNotFound),
            }
        };

        trace!("acquiring client");
        pool.get().await.map_err(|_| DBError::GetClient)
    }
}

#[allow(clippy::cognitive_complexity)]
pub(crate) fn starter(_: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering modules");
    let mut module_config: DBConfig = config.typed_module_config(module_path!())?;

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

        let Ok(pool) = dbcfg.create_pool(Some(Runtime::Tokio1), NoTls) else {
            error!("couldn't create database pool for {name}");
            continue;
        };

        dbc.insert(name.to_owned(), pool);
    }
    drop(dbc);

    let (tx, rx) = mpsc::channel::<ConsumerEvent>(1);
    let db = ModuleInfo {
        name: "dbstatus".s(),
        help: "check status of database connections".s(),
        acl: vec![],
        trigger: TriggerType::Keyword(vec!["db".s()]),
        channel: tx,
        error_prefix: Some("error getting database status".s()),
    };
    db.spawn(rx, module_config, dbstatus);

    Ok(vec![db])
}

/// Check status of database connections in the chat room.
///
/// # Errors
/// Will return `Err` if acquiring a connection pool, or sending response, fails.
pub async fn dbstatus(event: ConsumerEvent, config: DBConfig) -> anyhow::Result<()> {
    let mut wip_response: String = "database status:".to_string();

    trace!("attempting to grab dbc lock");
    for name in config.keys() {
        trace!("checking connection: {name}");
        let dbpool = DBPools::get_pool(name)?;

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

    event
        .room
        .send(RoomMessageEventContent::text_plain(wip_response))
        .await?;

    Ok(())
}

/// Database pool errors
#[derive(Debug)]
pub enum DBError {
    /// Couldn't acquire connection collection lock.
    CollectionLock,
    /// Database known under the handle not found in configuration.
    HandleNotFound,
    /// Acquiring database client from the pool failed.
    GetClient,
}

impl StdError for DBError {}

impl fmt::Display for DBError {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::CollectionLock => write!(fmt, "Couldn't acquire connection collection lock"),
            Self::HandleNotFound => write!(fmt, "Handle not found in connections"),
            Self::GetClient => write!(fmt, "Couldn't get client from pool"),
        }
    }
}
