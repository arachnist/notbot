//! Provides interfaces to query prometheus-like tsdb's.

use crate::prelude::*;
use js_int::uint;
use matrix_sdk::attachment::{AttachmentConfig, AttachmentInfo, BaseImageInfo};

use askama::Template;
use chrono::{DateTime, Utc};
use plotters::prelude::*;
use serde::Deserialize;
use tempfile::Builder;

/// Configuration for known VictoriaMetrics/Prometheus instances
#[derive(Clone, Debug, Deserialize)]
pub struct PromQueryConfig {
    /// Named queries
    pub queries: HashMap<String, Query>,
    /// Named instances and their names
    pub instances: HashMap<String, String>,
    #[serde(default = "keywords")]
    /// Keywords the query module will respond to. Default are `prom`, `env`, and `env-dc`
    /// If a keyword matches a defined query name, that query is used.
    pub keywords: Vec<String>,
    /// Default directory for temporary graph images. Default is `./`
    #[serde(default = "graph_tmp_dir")]
    pub graph_tmp_dir: String,
}

fn keywords() -> Vec<String> {
    vec!["prom".s(), "env".s(), "env-dc".s()]
}

fn graph_tmp_dir() -> String {
    "./".s()
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

    let instance = config.instances.get(&q.instance).map_or(&q.instance, |i| i);

    let client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("building http client:")?;

    let params = [("query", q.query.clone())];

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

    let plain = data.render()?;
    let message = RoomMessageEventContent::text_plain(plain);

    event.room.send(message).await?;

    Ok(())
}

async fn graph(event: ConsumerEvent, config: PromQueryConfig) -> anyhow::Result<()> {
    let Some(maybe_args) = event.args else {
        bail!("missing arguments: <query> [time range]")
    };

    let mut args = maybe_args.split_whitespace();

    let Some(maybe_query_name) = args.next() else {
        bail!("missing or arguments: <query> [time range]")
    };
    let Some(query) = config.queries.get(maybe_query_name) else {
        bail!("no configured query matched")
    };
    let instance = config
        .instances
        .get(&query.instance)
        .map_or(&query.instance, |i| i);

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

    let normalized = data.normalize();

    for metric in normalized {
        let named_tempfile = Builder::new()
            .prefix("notbot-prom-")
            .suffix(".png")
            .rand_bytes(5)
            .tempfile_in(config.graph_tmp_dir.clone())?;

        match QueryData::graph(&named_tempfile, &metric) {
            Ok(()) => (),
            Err(e) => {
                error!("graphing failed: {e}");
                continue;
            }
        };

        let image = fs::read(named_tempfile)?;

        let attachment_config = AttachmentConfig::new()
            .caption(metric.0.get("property").cloned())
            .info(AttachmentInfo::Image(BaseImageInfo {
                height: Some(uint!(330)),
                width: Some(uint!(1010)),
                ..Default::default()
            }));

        trace!("sending image");

        event
            .room
            .send_attachment("graph.png", &mime::IMAGE_PNG, image, attachment_config)
            .await?;
    }

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
    /// Normalizes the returned `SeriesVariant` into a more Matrix-like structure.
    ///
    /// Attempts to parse the timestamps and values in the returned datapoints, and will
    /// skip any that don't parse. This may result in returned values vector being empty.
    ///
    /// # TODO:
    /// It would probably be better to normalize the datapoints when deserializing, with
    /// a serde visitor.
    #[must_use]
    #[allow(clippy::type_complexity)]
    pub fn normalize(&self) -> (HashMap<String, String>, Vec<(DateTime<Utc>, f64)>) {
        match self {
            Self::Vector { metric, value } => Self::normalize_datapoint(value)
                .map_or_else(|| (metric.clone(), vec![]), |dp| (metric.clone(), vec![dp])),
            Self::Matrix { metric, values } => {
                let mut rval: Vec<(DateTime<Utc>, f64)> = vec![];

                for value in values {
                    if let Some(dp) = Self::normalize_datapoint(value) {
                        rval.push(dp);
                    }
                }

                (metric.clone(), rval)
            }
        }
    }

    fn normalize_datapoint(dp: &(f64, String)) -> Option<(DateTime<Utc>, f64)> {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let secs = dp.0.trunc() as i64;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
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
    #[must_use]
    #[allow(clippy::type_complexity)]
    pub fn normalize(&self) -> Vec<(HashMap<String, String>, Vec<(DateTime<Utc>, f64)>)> {
        let mut retv = vec![];

        for elem in &self.result {
            retv.push(elem.normalize());
        }

        retv
    }

    /// Graph the returned timeseries data.
    /// # Errors
    /// Will return `Err` if provided data is empty, it's impossible
    /// to find minimum/maximum values, or drawing operations fail.
    #[allow(clippy::type_complexity)]
    pub fn graph(
        named_tempfile: &tempfile::NamedTempFile,
        normalized: &(HashMap<String, String>, Vec<(DateTime<Utc>, f64)>),
    ) -> anyhow::Result<()> {
        let root = BitMapBackend::new(&named_tempfile, (1010, 330)).into_drawing_area();
        root.fill(&WHITE)?;

        trace!("file: {named_tempfile:#?}");

        let Some(first) = normalized.1.first() else {
            bail!("empty data")
        };
        let Some(last) = normalized.1.last() else {
            bail!("empty data")
        };

        #[allow(clippy::cast_possible_truncation)]
        let min = normalized
            .1
            .iter()
            .min_by_key(|e| e.1 as i64 - 1)
            .ok_or_else(|| anyhow!("cannot find minimum value in data"))?;
        #[allow(clippy::cast_possible_truncation)]
        let max = normalized
            .1
            .iter()
            .max_by_key(|e| e.1 as i64 + 1)
            .ok_or_else(|| anyhow!("cannot find maximum value in data"))?;

        let mut chart = ChartBuilder::on(&root)
            .margin(10)
            .set_label_area_size(LabelAreaPosition::Left, 30)
            .set_label_area_size(LabelAreaPosition::Bottom, 30)
            .build_cartesian_2d(first.0..last.0, min.1..max.1)?;

        chart.configure_mesh().x_labels(8).y_labels(5).draw()?;

        chart.draw_series(LineSeries::new(normalized.1.clone(), &BLUE))?;

        root.present()?;

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
