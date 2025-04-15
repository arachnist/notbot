use crate::{Config, WorkerStarter, WORKERS};

use std::collections::HashMap;

use serde::Deserialize;
use tokio::task::AbortHandle;

use matrix_sdk::Client;

use linkme::distributed_slice;

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct ModuleConfig {
	listen_address: String,
}

#[distributed_slice(WORKERS)]
static WORKER_STARTER: WorkerStarter = (module_path!(), worker_starter);

fn worker_starter(_: &Client, config: &Config) -> anyhow::Result<AbortHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
    let worker = tokio::task::spawn(worker_entrypoint(module_config));
    Ok(worker.abort_handle())
}

async fn worker_entrypoint(module_config: ModuleConfig) {
}