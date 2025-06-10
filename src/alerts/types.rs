//! Type definitions for the alerts module.

use crate::prelude::{Deserialize, ToStringExt};

use std::collections::HashMap;

/// Configuration for a single grafana instance
#[derive(Clone, Debug, Deserialize)]
pub struct GrafanaConfig {
    /// instance name
    pub name: String,
    /// bearer token it will use when firing webhooks
    pub token: String,
    /// matrix rooms to which the alert should be forwarded to
    pub rooms: Vec<String>,
}

/// Module configurations
#[derive(Clone, Debug, Deserialize)]
pub struct ModuleConfig {
    /// Map of grafana instances
    pub grafanas: HashMap<String, GrafanaConfig>,
    /// Keywords to which bot will respond with list of known firing alerts, with a message per instance with firing alerts
    #[serde(default = "keywords_alerting")]
    pub keywords_alerting: Vec<String>,
    /// keywords on which bot will purge known alerts.
    #[serde(default = "keywords_purge")]
    pub keywords_purge: Vec<String>,
    /// rooms on which admins will be able to request purging the list of known alerts
    pub rooms_purge: Vec<String>,
    #[serde(default = "no_firing_alerts_responses")]
    /// possible messages to respond with if no alerts are firing
    pub no_firing_alerts_responses: Vec<String>,
}

fn keywords_alerting() -> Vec<String> {
    vec!["alerting".s(), "alerts".s()]
}

fn keywords_purge() -> Vec<String> {
    vec!["purge".s(), "alerts_purge".s()]
}

fn no_firing_alerts_responses() -> Vec<String> {
    vec!["all systems operational".s()]
}
