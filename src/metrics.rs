//! Gather metrics about the bot functions, and served http requests and expose metrics endpoint.
//!
//! Metrics exposed by the bot include:
//! * number of events consumed by bot modules.
//! * general tokio runtime statistics
//! * general http statistics
//! * general process statistics
//!
//! Specific modules may also define their own metrics using the [`prometheus`] crate:
//! ```
//! use std::sync::LazyLock;
//! use prometheus::{opts, register_int_counter_vec, IntCounterVec};
//!
//! static MODULE_EVENTS: LazyLock<IntCounterVec> = LazyLock::new(|| {
//!     register_int_counter_vec!(
//!         opts!(
//!             "module_event_counts",
//!             "Number of events a module has consumed"
//!         ),
//!         &["event"]
//!     )
//!     .unwrap()
//! });
//! ```
//!
//! The macro provided by [`prometheus`] crate automatically registers the metrics for exposure on the metrics endpoint.
//!
//! # Configuration
//!
//! None, this module is used directly by [`crate::webterface`]
//!
//! # Usage
//!
//! Point your favourite prometheus-compatible metrics consumer at the bot. Ad-hoc calls might also be useful in development.
//!
//! ```text
//! ❯ curl --silent https://notbot.is-a.cat/metrics | grep '^process'
//! process_cpu_seconds_total 60
//! process_max_fds 1024
//! process_open_fds 27
//! process_resident_memory_bytes 74407936
//! process_start_time_seconds 1746860343
//! process_threads 34
//! process_virtual_memory_bytes 2567520256
//! ```

use crate::prelude::*;

use axum::{
    body::HttpBody, extract::Request, http::StatusCode, middleware::Next, response::IntoResponse,
};

use prometheus::{opts, register_histogram_vec, register_int_counter_vec};
use prometheus::{HistogramVec, IntCounterVec, TextEncoder};

static HTTP_COUNTER: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!("http_requests_total", "Number of HTTP requests made."),
        &["method", "status"]
    )
    .unwrap()
});
static HTTP_BODY_HISTOGRAM: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        "http_response_size_bytes",
        "The HTTP response lower bound sizes in bytes.",
        &["method", "status"],
        vec![10.0, 100.0, 1000.0, 10000.0, 100000.0]
    )
    .unwrap()
});
static HTTP_REQ_HISTOGRAM: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        "http_request_duration_seconds",
        "The HTTP request latencies in seconds.",
        &["method", "status"]
    )
    .unwrap()
});

pub(crate) async fn serve_metrics() -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    let body = encoder.encode_to_string(&metric_families).map_err(|err| {
        error!("Failed encoding metrics as text: {:?}", err);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed encoding metrics as text.",
        )
    })?;

    Ok(body)
}

pub(crate) async fn track_metrics(req: Request, next: Next) -> impl IntoResponse {
    let method: String = req.method().clone().to_string();
    let timer = Instant::now();

    let response = next.run(req).await;

    let latency = timer.elapsed().as_secs_f64();

    let status = response.status().as_u16().to_string();
    let response_size: f64 = response.body().size_hint().lower() as f64;

    HTTP_COUNTER
        .with_label_values(&[method.clone(), status.clone()])
        .inc();
    HTTP_REQ_HISTOGRAM
        .with_label_values(&[method.clone(), status.clone()])
        .observe(latency);
    HTTP_BODY_HISTOGRAM
        .with_label_values(&[method, status])
        .observe(response_size);
    response
}
