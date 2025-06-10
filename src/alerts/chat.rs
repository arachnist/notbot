//! Chat interface for interacting with the alerts module.

use super::types::{GrafanaConfig, ModuleConfig};
use super::grafana;

use std::time::UNIX_EPOCH;

use crate::prelude::ConsumerEvent;

use crate::prelude::{trace, anyhow, bail, SystemTime, RoomMessageEventContent};

/// Removes entries from the list of known alerts.
///
/// Also, a perfect example of how using acls and triggers reduces the amount of code.
/// # Errors
/// Will return `Err` if:
/// * purging inner state fails
/// * sending response fails
pub async fn purge_processor(ev: ConsumerEvent, _: ModuleConfig) -> anyhow::Result<()> {
    trace!("purging alerts");
    let response = match grafana::FIRING_ALERTS.purge() {
        Ok(()) => "alerts purged",
        Err(e) => return Err(e),
    };

    ev.room
        .send(RoomMessageEventContent::text_plain(response))
        .await?;

    Ok(())
}

/// Handles requests to display current status of known alerts
///
/// # Errors
/// Will return `Err` if:
/// * argument is provided but is either malformed, or doesn't match a known grafana instance
/// * sending responses fails
/// * module is misconfigured and configuration deserializing didn't catch this.
pub async fn alerting_processor(event: ConsumerEvent, config: ModuleConfig) -> anyhow::Result<()> {
    let mut grafanas: Vec<GrafanaConfig> = vec![];
    let mut sent: bool = false;

    if let Some(maybe_grafana_instances) = event.args {
        trace!("maybe instances: {maybe_grafana_instances}");
        let mut maybe_grafanas: Vec<String> = vec![];
        let mut args = maybe_grafana_instances.split_whitespace();

        let first = args
            .next()
            .ok_or_else(|| anyhow!("missing arguments"))?
            .to_string();

        maybe_grafanas.push(first);

        for maybe_grafana in args {
            maybe_grafanas.push(maybe_grafana.to_string());
        }

        for instance_name in maybe_grafanas {
            if let Some(grafana) = config.grafanas.get(&instance_name) {
                grafanas.push(grafana.clone());
            } else {
                bail!("provided grafana instance is not known: {instance_name}");
            };
        }
    } else {
        grafanas = config.grafanas.values().cloned().collect();
        trace!("all instances: {grafanas:#?}");
    }

    trace!("grafanas to check: {grafanas:#?}");

    for grafana in grafanas {
        let name = grafana.name.as_str();
        let alerts = grafana::FIRING_ALERTS.get(name);
        match alerts {
            None => {
                trace!("no alerts known");
            }
            Some(va) => {
                if va.is_empty() {
                    continue;
                };
                event.room.send(grafana::to_matrix_message(va, name)).await?;
                sent = true;
            }
        };
    }

    if !sent {
        let mut response = String::new();
        config
            .no_firing_alerts_responses
            .first()
            .ok_or_else(|| anyhow!("module misconfigured: missing `ok` responses"))?
            .clone_into(&mut response);
        // same hack as crate::module::dispatch_module()
        if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
            let milis = now.as_millis();
            // FIXME: sketchy AF
            let chosen_idx: usize = milis as usize % config.no_firing_alerts_responses.len();
            if let Some(option) = config.no_firing_alerts_responses.get(chosen_idx) {
                option.clone_into(&mut response);
            };
        };

        event
            .room
            .send(RoomMessageEventContent::text_plain(response))
            .await?;
    };

    Ok(())
}
