//! Send alerts to the bot from grafana instances.
//!
//! # Configuration
//!
//! Entries under `grafanas` are a map of strings to grafana instance configurations.
//!
//! [`ModuleConfig`]
//!
//! ```toml
//! [module."notbot::alerts".grafanas.hswaw]
//! name = "hswaw"
//! token = "…"
//! rooms = [
//!     "#infra:example.org",
//!     "#bottest:example.com",
//!     "#notbot-test-private-room:example.com",
//! ]
//!
//! [module."notbot::alerts".grafanas.cat]
//! name = "cat"
//! token = "…"
//! rooms = [
//!     "#bottest:example.com",
//!     "#notbot-test-private-room:example.com",
//! ]
//!
//! [module."notbot::alerts"]
//! rooms_purge = [
//!     "#bottest:example.org",
//!     "!xnhydwPoIQeoVuJCaU:example.com",
//! ]
//! no_firing_alerts_responses = [
//!     "all systems operational",
//!     "all crews reporting",
//!     "battlecruiser operational",
//! ]
//! keywords_alerting = [ "alerting", "alerts" ]
//! keywords_purge = [ "purge", "alerts_purge" ]
//! ```
//!
//! # Usage
//!
//! Keywords the module will respond to:
//! * `alerting`, `alerts` - list currently firing alerts. [`alerting_processor`]
//! * `purge`, `alerts_purge` - empty the lists of known alerts [`purge_processor`]
//!
//! Urls the module will handle:
//! * `/hook/alerts` - handle incoming webhooks from grafana. [`receive_alerts`]

pub mod chat;
pub mod grafana;
pub mod types;

use chat::{alerting_processor, purge_processor};
use types::ModuleConfig;

use crate::prelude::{Acl, Config, ModuleInfo, TriggerType};

use crate::prelude::info;

use matrix_sdk::Client;

pub(crate) fn starter(_: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering grafana modules");
    let module_config: ModuleConfig = config.typed_module_config(module_path!())?;

    Ok(vec![
        ModuleInfo::new(
            "alerting",
            "shows which alerts are now firing",
            vec![],
            TriggerType::Keyword(module_config.keywords_alerting.clone()),
            Some("error"),
            module_config.clone(),
            alerting_processor,
        ),
        ModuleInfo::new(
            "alerts_purge",
            "reset the firing alerts to empty state",
            vec![Acl::Room(module_config.rooms_purge.clone())],
            TriggerType::Keyword(module_config.keywords_purge.clone()),
            Some("error purging state"),
            module_config,
            purge_processor,
        ),
    ])
}
