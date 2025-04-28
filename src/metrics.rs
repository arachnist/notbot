use std::time::Instant;

use hyper::body::Body;

use lazy_static::lazy_static;

use axum::{extract::Request, http::StatusCode, middleware::Next, response::IntoResponse};
use tracing::error;

use prometheus::{opts, register_histogram_vec, register_int_counter_vec};
use prometheus::{HistogramVec, IntCounterVec, TextEncoder};

lazy_static! {
    static ref HTTP_COUNTER: IntCounterVec = register_int_counter_vec!(
        opts!("http_requests_total", "Number of HTTP requests made."),
        &["method", "status"]
    )
    .unwrap();
    static ref HTTP_BODY_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "http_response_size_bytes",
        "The HTTP response lower bound sizes in bytes.",
        &["method", "status"],
        vec![10.0, 100.0, 1000.0, 10000.0, 100000.0]
    )
    .unwrap();
    static ref HTTP_REQ_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "http_request_duration_seconds",
        "The HTTP request latencies in seconds.",
        &["method", "status"]
    )
    .unwrap();
}

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
