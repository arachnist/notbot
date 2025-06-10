//! Grafana webhook payload structure.

use super::types::ModuleConfig;

use crate::prelude::{
    Arc, AuthBearer, Deserialize, HashMap, LazyLock, Mutex, RoomMessageEventContent, Serialize, ToStringExt,
    WebAppState, bail, maybe_get_room, trace,
};

use std::fmt;

use matrix_sdk::ruma::events::MessageLikeEventContent;

use axum::extract::State;
use axum::{Json, http::StatusCode, response::IntoResponse};
use serde_json::Value;

pub(crate) static FIRING_ALERTS: LazyLock<FiringAlerts> = LazyLock::new(Default::default);

/// Handles incoming webhooks from grafana instances.
///
/// Matches bearer tokens to known instances, updates state of known alerts, and dispatches alerts to matrix rooms accordingly.
///
/// # Errors
/// Will return `Err` if:
/// * module is misconfigured (missing auth configuration)
/// * gets called with unknown token
/// * modifying inner list of alert states fails
/// * sending room notifications fails
#[axum::debug_handler]
pub async fn receive_alerts(
    State(app_state): State<WebAppState>,
    AuthBearer(token): AuthBearer,
    Json(alerts): Json<Alerts>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    use AlertStatus::{Firing, Resolved};
    let module_config: ModuleConfig = {
        match app_state.config.typed_module_config("notbot::alerts") {
            Err(_) => return Err((StatusCode::INTERNAL_SERVER_ERROR, "no auth configuration")),
            Ok(v) => v,
        }
    };

    let mut maybe_instance: Option<String> = None;

    for (name, config) in &module_config.grafanas {
        if token == config.token {
            maybe_instance = Some(name.to_owned());
            break;
        }
    }

    let Some(instance) = maybe_instance else {
        return Err((StatusCode::FORBIDDEN, "unknown token"));
    };

    trace!("received hook body: {:#?}", alerts);

    let changed = match alerts.status {
        Firing => FIRING_ALERTS
            .fire(&instance, alerts.alerts)
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to fire alerts")),
        Resolved => FIRING_ALERTS
            .resolve(&instance, alerts.alerts)
            .map_err(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "failed to resolve alerts",
                )
            }),
    };

    trace!("{changed:#?}");
    if let Ok(alerts) = changed {
        if alerts.is_empty() {
            return Ok(());
        };
        async {
            if let Some(grafana_config) = module_config.grafanas.get(&instance) {
                for room in grafana_config.rooms.clone() {
                    if let Ok(mx_room) = maybe_get_room(&app_state.mx, &room).await {
                        let mx_message = to_matrix_message(alerts.clone(), &instance);
                        if let Err(e) = mx_room.send(mx_message).await {
                            trace!("failed to send room notification: {e}");
                        }
                    }
                }
            };

            Ok(())
        }
        .await
        .map_err(|_: anyhow::Error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to send room notifications",
            )
        })?;
    };

    Ok(())
}

/// Possible states of an alert.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
pub enum AlertStatus {
    /// Grafana informed us that alert conditions aren't satisfied
    #[serde(rename = "resolved")]
    Resolved,
    /// Grafana informed us that alert conditions are satisfied
    #[serde(rename = "firing")]
    #[default]
    Firing,
}

impl fmt::Display for AlertStatus {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        use AlertStatus::{Firing, Resolved};
        match self {
            Resolved => write!(fmt, "Resolved"),
            Firing => write!(fmt, "Firing"),
        }
    }
}

impl AlertStatus {
    pub(crate) const fn into_emoji(self) -> &'static str {
        use AlertStatus::{Firing, Resolved};
        match self {
            Firing => "🔥",
            Resolved => "🩷",
        }
    }
}

/// Container around Alert objects
#[allow(dead_code, missing_docs)]
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Alerts {
    pub receiver: String,
    pub status: AlertStatus,
    pub org_id: i64,
    pub alerts: Vec<Alert>,
    pub group_labels: HashMap<String, String>,
    pub common_labels: HashMap<String, String>,
    pub common_annotations: HashMap<String, String>,
    #[serde(rename = "externalURL")]
    pub external_url: String,
    pub version: String,
    pub group_key: String,
    pub truncated_alerts: i64,
    pub title: String,
    pub state: String,
    pub message: String,
}

/// Alert state definitions
#[allow(dead_code, missing_docs)]
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Alert {
    pub status: AlertStatus,
    pub labels: HashMap<String, String>,
    pub annotations: HashMap<String, String>,
    pub starts_at: String,
    pub ends_at: String,
    #[serde(rename = "generatorURL")]
    pub generator_url: String,
    pub fingerprint: String,
    #[serde(rename = "silenceURL")]
    pub silence_url: String,
    #[serde(rename = "dashboardURL")]
    pub dashboard_url: String,
    #[serde(rename = "panelURL")]
    pub panel_url: String,
    pub values: Value,
}

#[derive(Default)]
pub(crate) struct FiringAlerts {
    inner: Arc<Mutex<HashMap<String, Vec<Alert>>>>,
}

impl FiringAlerts {
    fn fire(&self, name: &str, alerts: Vec<Alert>) -> anyhow::Result<Vec<Alert>> {
        trace!("gathering alerts to fire");
        let mut inner = match self.inner.lock() {
            Ok(i) => i,
            Err(e) => bail!("failed locking alerts map: {e}"),
        };

        let mut changed: Vec<Alert> = vec![];

        trace!("listing known alerts");
        let known_alerts: Vec<String> = inner.get(name).map_or_else(
            || {
                changed.extend(alerts.clone());
                vec![]
            },
            |a| a.iter().map(|a| a.fingerprint.clone()).collect(),
        );

        trace!("adding unique firing alerts");
        inner
            .entry(name.to_owned())
            .and_modify(|va| {
                for a in alerts.clone() {
                    if !known_alerts.contains(&a.fingerprint) {
                        va.push(a.clone());
                        changed.push(a);
                    };
                }
            })
            .or_insert(alerts);
        drop(inner);
        Ok(changed)
    }

    fn resolve(&self, name: &str, alerts: Vec<Alert>) -> anyhow::Result<Vec<Alert>> {
        let mut inner = match self.inner.lock() {
            Ok(i) => i,
            Err(e) => bail!("failed locking alerts map: {e}"),
        };

        trace!("known instances: {:#?}", inner.keys());

        let resolved_fingerprints: Vec<String> =
            alerts.iter().map(|a| a.fingerprint.clone()).collect();

        inner
            .entry(name.to_owned())
            .and_modify(|va| va.retain(|a| !resolved_fingerprints.contains(&a.fingerprint)));
        drop(inner);

        Ok(alerts)
    }

    pub(crate) fn get(&self, name: &str) -> Option<Vec<Alert>> {
        let Ok(inner) = self.inner.lock() else {
            return None;
        };
        inner.get(name).map(std::borrow::ToOwned::to_owned)
    }

    // our known state has desynched for whatever reason, start from empty slate
    pub(crate) fn purge(&self) -> anyhow::Result<()> {
        if let Ok(mut inner) = self.inner.lock() {
            for instance in inner.values_mut() {
                instance.truncate(0);
            }
        } else {
            bail!("failed locking alerts map");
        };

        Ok(())
    }
}

/// Convert a vector of alerts into an html formatted matrix message.
#[must_use]
pub fn to_matrix_message(va: Vec<Alert>, instance: &str) -> impl MessageLikeEventContent {
    let mut response_html = format!("instance: <b>{instance}</b><br />");
    let mut response = format!("instance: {instance}\n");

    for alert in va {
        let mut annotations_html = "".s();
        for (key, value) in alert.annotations.clone() {
            annotations_html.push_str(format!("{key}: <b>{value}</b><br/>").as_str());
        }
        response_html.push_str(
            format!(
                r"{state_emoji}<b>{state}</b><br/>
{annotations}
since: {since}<br />",
                state_emoji = alert.status.clone().into_emoji(),
                state = alert.status,
                annotations = annotations_html,
                since = alert.starts_at,
            )
            .as_str(),
        );

        let mut annotations = "".s();
        for (key, value) in alert.annotations {
            annotations.push_str(format!("{key}: {value}\n").as_str());
        }
        response.push_str(
            format!(
                "{state_emoji} {state}\n
{annotations}since: {since}\n",
                state_emoji = alert.status.clone().into_emoji(),
                state = alert.status,
                annotations = annotations,
                since = alert.starts_at,
            )
            .as_str(),
        );
    }

    RoomMessageEventContent::text_html(response, response_html)
}
