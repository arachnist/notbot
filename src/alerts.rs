use crate::prelude::*;

use std::time::{SystemTime, UNIX_EPOCH};

use matrix_sdk::ruma::events::MessageLikeEventContent;
use serde_json::Value;

use axum::{
    extract::{Json, State},
    http::{header::AUTHORIZATION, request::Parts, StatusCode},
    response::IntoResponse,
};
use axum_core::extract::FromRequestParts;

static FIRING_ALERTS: LazyLock<FiringAlerts> = LazyLock::new(Default::default);

#[derive(Default)]
pub struct FiringAlerts {
    inner: Arc<Mutex<HashMap<String, Vec<Alert>>>>,
}

impl FiringAlerts {
    pub fn fire(&self, name: String, alerts: Vec<Alert>) -> anyhow::Result<Vec<Alert>> {
        trace!("gathering alerts to fire");
        let mut inner = match self.inner.lock() {
            Ok(i) => i,
            Err(e) => bail!("failed locking alerts map: {e}"),
        };

        let mut changed: Vec<Alert> = vec![];

        trace!("listing known alerts");
        let known_alerts: Vec<String> = match inner.get(&name) {
            None => { changed.extend(alerts.clone()); vec![] },
            Some(a) => a.iter().map(|a| a.fingerprint.clone()).collect(),
        };

        trace!("adding unique firing alerts");
        inner
            .entry(name)
            .and_modify(|va| {
                for a in alerts.clone() {
                    if !known_alerts.contains(&a.fingerprint) {
                        va.push(a.clone());
                        changed.push(a);
                    };
                }
            })
            .or_insert(alerts);

        trace!("inner status: {inner:#?}");
        Ok(changed)
    }

    pub fn resolve(&self, name: String, alerts: Vec<Alert>) -> anyhow::Result<Vec<Alert>> {
        let mut inner = match self.inner.lock() {
            Ok(i) => i,
            Err(e) => bail!("failed locking alerts map: {e}"),
        };

        trace!("known instances: {:#?}", inner.keys());

        let resolved_fingerprints: Vec<String> =
            alerts.iter().map(|a| a.fingerprint.clone()).collect();

        inner
            .entry(name)
            .and_modify(|va| va.retain(|a| !resolved_fingerprints.contains(&a.fingerprint)));

        Ok(alerts)
    }

    pub fn get(&self, name: String) -> Option<Vec<Alert>> {
        let inner = match self.inner.lock() {
            Ok(i) => i,
            Err(_) => return None,
        };

        trace!("known instances: {:#?}", inner.keys());

        match inner.get(&name) {
            None => return None,
            Some(va) => Some(va.to_owned()),
        }
    }

    // our known state has desynched for whatever reason, start from empty slate
    pub fn purge(&self) -> anyhow::Result<()> {
        let mut inner = match self.inner.lock() {
            Ok(i) => i,
            Err(e) => bail!("failed locking alerts map: {e}"),
        };

        trace!("known instances: {:#?}", inner.keys());

        for instance in inner.values_mut() {
            instance.truncate(0);
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct GrafanaConfig {
    name: String,
    token: String,
    rooms: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModuleConfig {
    grafanas: HashMap<String, GrafanaConfig>,
    #[serde(default = "keywords_alerting")]
    keywords_alerting: Vec<String>,
    #[serde(default = "keywords_purge")]
    keywords_purge: Vec<String>,
    rooms_purge: Vec<String>,
    #[serde(default = "no_firing_alerts_responses")]
    no_firing_alerts_responses: Vec<String>,
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

#[axum::debug_handler]
pub async fn receive_alerts(
    State(app_state): State<WebAppState>,
    AuthBearer(token): AuthBearer,
    Json(alerts): Json<Alerts>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    use AlertStatus::*;
    let module_config: ModuleConfig = {
        match app_state.bot_config.module_config_value(module_path!()) {
            Err(_) => return Err((StatusCode::INTERNAL_SERVER_ERROR, "no auth configuration")),
            Ok(v) => match v.try_into() {
                Err(_) => return Err((StatusCode::INTERNAL_SERVER_ERROR, "no auth configuration")),
                Ok(d) => d,
            },
        }
    };

    let mut instance: Option<String> = None;

    for (name, config) in module_config.grafanas.iter() {
        if token == config.token {
            instance = Some(name.to_owned());
            break;
        }
    }

    if instance.is_none() {
        return Err((StatusCode::FORBIDDEN, "unknown token"));
    };

    trace!("received hook body: {:#?}", alerts);

    let changed = match alerts.status {
        Firing => FIRING_ALERTS
            .fire("cat".s(), alerts.alerts)
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to fire alerts")),
        Resolved => FIRING_ALERTS
            .resolve("cat".s(), alerts.alerts)
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
            // can .unwrap() as the .is_none() case is handled above
            if let Some(grafana_config) = module_config.grafanas.get(&instance.unwrap()) {
                for room in grafana_config.rooms.clone() {
                    if let Ok(mx_room) = maybe_get_room(&app_state.mx, &room).await {
                        let mx_message = to_matrix_message(alerts.clone());
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

pub(crate) fn starter(_: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering grafana modules");
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;

    let (alerting_tx, alerting_rx) = mpsc::channel::<ConsumerEvent>(1);
    let alerting = ModuleInfo {
        name: "alerting".s(),
        help: "shows which alerts are now firing".s(),
        acl: vec![],
        trigger: TriggerType::Keyword(module_config.keywords_alerting.clone()),
        channel: Some(alerting_tx),
        error_prefix: Some("error".s()),
    };
    alerting.spawn(alerting_rx, module_config.clone(), alerting_processor);

    let (purge_tx, purge_rx) = mpsc::channel::<ConsumerEvent>(1);
    let purge = ModuleInfo {
        name: "alerts_purge".s(),
        help: "reset the firing alerts to empty state".s(),
        acl: vec![Acl::Room(module_config.rooms_purge.clone())],
        trigger: TriggerType::Keyword(module_config.keywords_purge.clone()),
        channel: Some(purge_tx),
        error_prefix: Some("error purging state".s()),
    };
    purge.spawn(purge_rx, module_config, purge_processor);

    Ok(vec![alerting, purge])
}

async fn purge_processor(_: ConsumerEvent, _: ModuleConfig) -> anyhow::Result<()> {
    trace!("purging alerts");
    FIRING_ALERTS.purge()
}

async fn alerting_processor(event: ConsumerEvent, config: ModuleConfig) -> anyhow::Result<()> {
    let mut grafanas: Vec<GrafanaConfig> = vec![];
    let mut sent: bool = false;

    if let Some(maybe_grafana_instances) = event.args {
        trace!("maybe instances: {maybe_grafana_instances}");
        let mut maybe_grafanas: Vec<String> = vec![];
        let mut args = maybe_grafana_instances.split_whitespace();

        let first = args.next().unwrap().to_string();

        maybe_grafanas.push(first);

        while let Some(maybe_grafana) = args.next() {
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
        let name = grafana.name.clone();
        let alerts = FIRING_ALERTS.get(name);
        match alerts {
            None => {
                trace!("no alerts known")
            }
            Some(va) => {
                event.room.send(to_matrix_message(va)).await?;
                sent = true;
            }
        };
    }

    if !sent {
        let mut response = config.no_firing_alerts_responses.get(0).unwrap().to_owned();
        // same hack as crate::module::dispatch_module()
        if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
            let milis = now.as_millis();
            // FIXME: sketchy AF
            let chosen_idx: usize = milis as usize % config.no_firing_alerts_responses.len();
            if let Some(option) = config.no_firing_alerts_responses.get(chosen_idx) {
                response = option.to_owned();
            };
        };

        event
            .room
            .send(RoomMessageEventContent::text_plain(response))
            .await?;
    };

    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub enum AlertStatus {
    #[serde(rename = "resolved")]
    Resolved,
    #[serde(rename = "firing")]
    #[default]
    Firing,
}

impl fmt::Display for AlertStatus {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        use AlertStatus::*;
        match self {
            Resolved => write!(fmt, "Resolved"),
            Firing => write!(fmt, "Firing"),
        }
    }
}

impl AlertStatus {
    pub fn into_emoji(&self) -> &'static str {
        use AlertStatus::*;
        match self {
            Firing => "🔥",
            Resolved => "🩷",
        }
    }
}

pub fn to_matrix_message(va: Vec<Alert>) -> impl MessageLikeEventContent {
    let mut response_html = "".s();
    let mut response = "".s();

    for alert in va.clone() {
        let mut annotations_html = "".s();
        for (key, value) in alert.annotations.clone() {
            annotations_html.push_str(format!("{key}: <b>{value}</b><br/>").as_str());
        }
        response_html.push_str(
            format!(
                r#"{state_emoji}<b>{state}</b><br/>
{annotations}
since: {since}<br />"#,
                state_emoji = alert.status.into_emoji(),
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
                state_emoji = alert.status.into_emoji(),
                state = alert.status,
                annotations = annotations,
                since = alert.starts_at,
            )
            .as_str(),
        );
    }

    RoomMessageEventContent::text_html(response, response_html)
}

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

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct AuthBearer(pub String);

impl<B> FromRequestParts<B> for AuthBearer
where
    B: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(req: &mut Parts, _: &B) -> Result<Self, Self::Rejection> {
        Self::decode_request_parts(req)
    }
}

impl AuthBeaererCustom for AuthBearer {
    const ERROR_CODE: StatusCode = StatusCode::FORBIDDEN;

    fn from_header(contents: &str) -> Self {
        Self(contents.to_string())
    }
}

pub trait AuthBeaererCustom: Sized {
    const ERROR_CODE: StatusCode;

    fn from_header(contents: &str) -> Self;

    fn decode_request_parts(req: &mut Parts) -> Result<Self, (StatusCode, &'static str)> {
        // Get authorization header
        let authorization = req
            .headers
            .get(AUTHORIZATION)
            .ok_or((Self::ERROR_CODE, "Authorization header missing"))?
            .to_str()
            .map_err(|_| (Self::ERROR_CODE, "Authorization header couldn't be decoded"))?;

        // Check that its a well-formed bearer and return
        let split = authorization.split_once(' ');
        match split {
            // Found proper bearer
            Some((name, contents)) if name == "Bearer" => Ok(Self::from_header(contents)),
            _ => Err((Self::ERROR_CODE, "Authorization header invalid")),
        }
    }
}

/*
 * The following was mostly developed against "Grafana OnCall", which was deprecated last month
 * https://grafana.com/blog/2025/03/11/grafana-oncall-maintenance-mode/
 * Leaving this here for posterity

#[derive(Clone, Debug, Deserialize)]
pub struct GrafanaConfig {
    name: String,
    url: String,
    secret: String,
}

pub(crate) fn starter(_: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering grafana modules");
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;

    // /api/v1/alerts/,
    let (alerting_tx, alerting_rx) = mpsc::channel::<ConsumerEvent>(1);
    let alerting = ModuleInfo {
        name: "alerting".s(),
        help: "shows which alerts are now firing".s(),
        acl: vec![],
        trigger: TriggerType::Keyword(module_config.keywords_alerting.clone()),
        channel: Some(alerting_tx),
        error_prefix: Some("error".s()),
    };
    alerting.spawn(alerting_rx, module_config, alerting_processor);
    Ok(vec![alerting])
}

async fn alerting_processor(event: ConsumerEvent, config: ModuleConfig) -> anyhow::Result<()> {
    let mut grafanas: Vec<GrafanaConfig> = vec![];
    let mut response: String = "".s();

    if let Some(maybe_grafana_instances) = event.args {
        let mut maybe_grafanas: Vec<String> = vec![];
        let mut args = maybe_grafana_instances.split_whitespace();

        let first = args.next().unwrap().to_string();

        maybe_grafanas.push(first);

        while let Some(maybe_grafana) = args.next() {
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
    }

    for instance in grafanas {
        let endpoint = format!("{base}/api/v1/alerts/", base = instance.url);
        let secret = instance.secret;
        let results: Vec<AlertsResult> =
            get_paginated_results::<Alerts, AlertsResult>(endpoint, secret).await?;

        let mut local_alerting: Vec<String> = vec![];

        for result in results {
            if result.payload.state == "alerting" {
                local_alerting.push(result.payload.title);
            }
        }

        response.push_str(
            format!(
                "instance {name}, alerting: {alerts}\n",
                name = instance.name,
                alerts = local_alerting.join(", "),
            )
            .as_str(),
        );
    }

    if response != "" {
        event
            .room
            .send(RoomMessageEventContent::text_plain(response))
            .await?;
    } else {
        let mut response = config.no_firing_alerts_responses.get(0).unwrap();
        // same hack as crate::module::dispatch_module()
        if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
            let milis = now.as_millis();
            // FIXME: sketchy AF
            let chosen_idx: usize = milis as usize % config.no_firing_alerts_responses.len();
            if let Some(option) = config.no_firing_alerts_responses.get(chosen_idx) {
                response = option;
            };
        };
        event
            .room
            .send(RoomMessageEventContent::text_plain(response))
            .await?;
    }

    Ok(())
}

pub trait HasResults<R>
where
    R: de::DeserializeOwned + Clone + Send + Sync,
{
    fn get_results(&self) -> Vec<R>;
}

pub trait IsPaginated {
    fn get_next(&self) -> Option<String>;
    #[allow(dead_code)]
    fn get_previous(&self) -> Option<String>;
}

impl HasResults<AlertsResult> for Alerts {
    fn get_results(&self) -> Vec<AlertsResult> {
        self.results.clone()
    }
}

impl IsPaginated for Alerts {
    fn get_next(&self) -> Option<String> {
        self.next.clone()
    }

    fn get_previous(&self) -> Option<String> {
        self.previous.clone()
    }
}

async fn get_paginated_results<D, R>(endpoint: String, secret: String) -> anyhow::Result<Vec<R>>
where
    R: de::DeserializeOwned + Clone + Send + Sync, /* + std::iter::Iterator<Item = R> */
    D: de::DeserializeOwned + Clone + Send + Sync + HasResults<R> + IsPaginated,
{
    let mut res: Vec<R> = vec![];
    let mut maybe_next = Some(endpoint);
    let bearer_value = header::HeaderValue::from_str(format!("Bearer {}", secret).as_str())?;
    let mut headers = header::HeaderMap::new();
    headers.insert("Authorization", bearer_value);
    let client = RClient::builder().default_headers(headers).build()?;

    while let Some(next) = maybe_next {
        let response = client.get(next).send().await?;
        let contents = response.text().await?;
        // trace!("response body: {}", response.clone().text().await?.clone());
        let data: D = serde_json::from_str(&contents).with_context(|| {
            format!("Unable to deserialise response. Body was: \"{}\"", contents)
        })?;
        maybe_next = data.get_next();

        res.extend(data.get_results());
    }
    Ok(res)
}

#[derive(Debug, Clone, Deserialize)]
pub struct Alerts {
    pub count: i64,
    pub next: Option<String>,
    pub previous: Option<String>,
    pub results: Vec<AlertsResult>,
    pub current_page_number: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlertsResult {
    pub id: String,
    pub alert_group_id: String,
    pub created_at: String,
    pub payload: AlertsPayload,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlertsPayload {
    pub state: String,
    pub title: String,
    pub rule_id: i64,
    pub message: String,
    pub rule_url: String,
    pub rule_name: String,
    pub eval_matches: Vec<AlertsEvalMatch>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlertsEvalMatch {
    pub tags: Value,
    pub value: i64,
    pub metric: String,
}
*/
