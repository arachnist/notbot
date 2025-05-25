//! Provides interfaces to query prometheus-like tsdb's.

use crate::prelude::*;
use askama::Template;
use chrono::{DateTime, Utc};
// use plotters::{prelude::*, style::full_palette::PURPLE_A400};
use serde::Deserialize;

/// Configuration for known VictoriaMetrics/Prometheus instances
#[derive(Clone, Debug, Deserialize)]
pub struct PromQueryConfig {
    /// Named queries
    pub queries: HashMap<String, Query>,
    /// Named instances and their names
    pub instances: HashMap<String, String>,
    #[serde(default = "keywords")]
    /// Keywords the module will respond to. Default are `prom`, `env`, and `env-dc`
    /// If a keyword matches a defined query name, that query is used.
    pub keywords: Vec<String>,
}

fn keywords() -> Vec<String> {
    vec!["prom".s(), "env".s(), "env-dc".s()]
}

/// Actual named query.
#[derive(Clone, Debug, Deserialize)]
pub struct Query {
    /// Instance on which this query is supposed to be run.
    pub instance: String,
    /// The actual Query
    pub query: String,
}

pub(crate) fn starter(_: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    let module_config: PromQueryConfig = config.typed_module_config(module_path!())?;

    Ok(vec![
        ModuleInfo::new(
            "prom",
            "query a prometheus-like tsdb",
            vec![],
            TriggerType::Keyword(module_config.clone().keywords),
            Some("querying data source failed"),
            module_config.clone(),
            query,
        ),
        ModuleInfo::new(
            "graph",
            "graph data from prometheus-like tsdb",
            vec![],
            TriggerType::Keyword(vec!["graph".s()]),
            Some("querying data source failed"),
            module_config,
            graph,
        ),
    ])
}

async fn query(event: ConsumerEvent, config: PromQueryConfig) -> anyhow::Result<()> {
    let q = match config.queries.get(&event.keyword) {
        Some(q) => q,
        None => {
            if let Some(query_name) = event.args.map(|e| e.trim().to_owned()) {
                if let Some(query) = config.queries.get(&query_name) {
                    query
                } else {
                    bail!("no configured query matched");
                }
            } else {
                bail!("no configured query matched");
            }
        }
    };

    let instance = match config.instances.get(&q.instance) {
        Some(i) => i,
        None => &q.instance,
    };

    let client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("building http client:")?;

    let params = [("query", q.query.clone())];
    trace!(
        "query: {params:#?}, client: {client:#?}, instance: {:#?}",
        q.instance.clone()
    );

    let response: QueryResponse = client
        .post(instance)
        .form(&params)
        .send()
        .await
        .context("error sending request")?
        .json()
        .await
        .context("error decoding request")?;

    let data = match response {
        QueryResponse::Success { data, .. } => data,
        QueryResponse::Error { error_type, error } => bail!("query failed: {error_type}: {error}"),
    };

    let plain = data.as_plain().render()?;
    let formatted = data.as_formatted().render()?;
    let message = RoomMessageEventContent::text_html(plain, formatted);

    event.room.send(message).await?;

    Ok(())
}

async fn graph(event: ConsumerEvent, config: PromQueryConfig) -> anyhow::Result<()> {
    let maybe_args = match event.args {
        Some(a) => a,
        None => bail!("missing arguments: <query> [time range]"),
    };

    let mut args = maybe_args.trim().split_whitespace();

    let maybe_query_name = match args.next() {
        Some(n) => n,
        None => bail!("missing or arguments: <query> [time range]"),
    };

    let query = match config.queries.get(maybe_query_name) {
        Some(q) => q,
        None => bail!("no configured query matched"),
    };

    let instance = match config.instances.get(&query.instance) {
        Some(i) => i,
        None => &query.instance,
    };

    let mut tr_parts = vec![];

    for a in args {
        tr_parts.push(a);
    }

    let time_range = if tr_parts.is_empty() {
        "[24h]".s()
    } else {
        tr_parts.join(" ")
    };

    let query_str = format!("{}{}", query.query, time_range);

    let params = [("query", query_str)];

    let client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("building http client:")?;

    let response: QueryResponse = client
        .post(instance)
        .form(&params)
        .send()
        .await?
        .json()
        .await?;

    let data = match response {
        QueryResponse::Success { data, .. } => data,
        QueryResponse::Error { error_type, error } => bail!("query failed: {error_type}: {error}"),
    };

    data.graph()?;

    event
        .room
        .send(RoomMessageEventContent::text_plain(
            "sorry, WIP. but if we got here, at least the query worked",
        ))
        .await?;

    Ok(())
}

/// Types of results.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum QueryResultType {
    /// Results are a vector.
    Vector,
    /// Results are a list of lists
    Matrix,
}

/// Types of returned series data format, depends on result type.
#[derive(Deserialize, Debug)]
#[serde(untagged)]
pub enum SeriesVariant {
    /// single timestamp-value pair per returned metric
    Vector {
        /// metric metadata, like labels
        metric: HashMap<String, String>,
        /// returned timestamp-value pair
        value: (f64, String),
    },
    /// list of timestamp-value pairs for each returned metric
    Matrix {
        /// metric metadata, like labels
        metric: HashMap<String, String>,
        /// list of returned timestamp-value pairs
        values: Vec<(f64, String)>,
    },
}

impl SeriesVariant {
    /// Normalizes the returned SeriesVariant into a more Matrix-like structure.
    ///
    /// Attempts to parse the timestamps and values in the returned datapoints, and will
    /// skip any that don't parse. This may result in returned values vector being empty.
    ///
    /// # TODO:
    /// It would probably be better to normalize the datapoints when deserializing, with
    /// a serde visitor.
    pub fn normalize(self) -> (HashMap<String, String>, Vec<(DateTime<Utc>, f64)>) {
        match self {
            SeriesVariant::Vector { metric, value } => {
                if let Some(dp) = Self::normalize_datapoint(&value) {
                    (metric, vec![dp])
                } else {
                    (metric, vec![])
                }
            }
            SeriesVariant::Matrix { metric, values } => {
                let mut rval: Vec<(DateTime<Utc>, f64)> = vec![];

                for value in values {
                    if let Some(dp) = Self::normalize_datapoint(&value) {
                        rval.push(dp);
                    }
                }

                (metric, rval)
            }
        }
    }

    fn normalize_datapoint(dp: &(f64, String)) -> Option<(DateTime<Utc>, f64)> {
        let secs = dp.0.trunc() as i64;
        let nsecs = (dp.0.fract() * 1_000_000_000_f64) as u32;

        let dt = DateTime::from_timestamp(secs, nsecs)?;
        let value = dp.1.parse().ok()?;

        Some((dt, value))
    }
}

/// Defines type of returned results, and contains them
#[derive(Template)]
#[template(
    path = "matrix/prom_query.html",
    blocks = ["formatted", "plain"],
)]
#[derive(Deserialize, Debug)]
pub struct QueryData {
    /// Type of results.
    #[serde(rename = "resultType")]
    pub result_type: QueryResultType,
    /// Result data.
    pub result: Vec<SeriesVariant>,
}

impl QueryData {
    /// Normalizes the data to equivalent of the Matrix variant to make things easier to work with.
    pub fn normalize(self) -> Vec<(HashMap<String, String>, Vec<(f64, f64)>)> {
        let mut retv = vec![];

        for elem in self.result {
            match elem {
                SeriesVariant::Vector { metric, value, .. } => retv.push((metric, vec![value])),
                SeriesVariant::Matrix { metric, values, .. } => retv.push((metric, values)),
            }
        }

        vec![]
    }

    /// Graph the returned timeseries data.
    pub fn graph(self) -> anyhow::Result<()> {
        let _data: Vec<(HashMap<String, String>, Vec<(f64, f64)>)> = self.normalize();

        Ok(())
    }
}

/// Statistics about the query response.
#[derive(Deserialize, Debug)]
pub struct QueryStats {
    /// Number of returned time series. Is actually `usize` inside the string.
    #[serde(rename = "seriesFetched")]
    pub series_fetched: String,
    /// How long did it take to execute query
    #[serde(rename = "executionTimeMsec")]
    pub execution_time_msec: u64,
}

/// Status of returned query results.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
#[serde(tag = "status")]
pub enum QueryResponse {
    /// Query was successful.
    Success {
        /// Data returned from the query,
        data: QueryData,
        /// Optionally returned query statistics
        stats: Option<QueryStats>,
    },
    /// Query encountered an error.
    Error {
        /// Type of returned error
        error_type: String,
        /// Error description
        error: String,
    },
}
