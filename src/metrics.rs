use crate::{Config, WorkerStarter, WORKERS};

use std::net::SocketAddr;

use serde::Deserialize;
use tokio::task::AbortHandle;

use matrix_sdk::Client;

use linkme::distributed_slice;

use hyper::body::Incoming;
use hyper::header::CONTENT_TYPE;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::Request;
use hyper::Response;
use hyper_util::rt::TokioIo;
use lazy_static::lazy_static;
use prometheus::{labels, opts, register_counter, register_gauge, register_histogram_vec};
use prometheus::{Counter, Encoder, Gauge, HistogramVec, TextEncoder};
use tokio::net::TcpListener;
use tracing::{error, warn, info};

type BoxedErr = Box<dyn std::error::Error + Send + Sync + 'static>;

lazy_static! {
    static ref HTTP_COUNTER: Counter = register_counter!(opts!(
        "example_http_requests_total",
        "Number of HTTP requests made.",
        labels! {"handler" => "all",}
    ))
    .unwrap();
    static ref HTTP_BODY_GAUGE: Gauge = register_gauge!(opts!(
        "example_http_response_size_bytes",
        "The HTTP response sizes in bytes.",
        labels! {"handler" => "all",}
    ))
    .unwrap();
    static ref HTTP_REQ_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "example_http_request_duration_seconds",
        "The HTTP request latencies in seconds.",
        &["handler"]
    )
    .unwrap();
}

async fn serve_req(_req: Request<Incoming>) -> Result<Response<String>, BoxedErr> {
    let encoder = TextEncoder::new();

    HTTP_COUNTER.inc();
    let timer = HTTP_REQ_HISTOGRAM.with_label_values(&["all"]).start_timer();

    let metric_families = prometheus::gather();
    let body = encoder.encode_to_string(&metric_families)?;
    HTTP_BODY_GAUGE.set(body.len() as f64);

    let response = Response::builder()
        .status(200)
        .header(CONTENT_TYPE, encoder.format_type())
        .body(body)?;

    timer.observe_duration();

    Ok(response)
}

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

// FIXME: can fail if the connection is held open by client
// FIXME: only handles a single connection at a time
async fn worker_entrypoint(module_config: ModuleConfig) {
    let addr: SocketAddr = match module_config.listen_address.parse() {
        Ok(a) => a,
        Err(e) => {
            error!("parsing config listen address failed: {e}");
            return ;
        }
    };

    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("binding to listen socket failed: {e}");
            return ;
        }
    };

    info!("listening on {addr}");

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                warn!("accepting socket failed: {e}");
                continue;
            },
        };
        let io = TokioIo::new(stream);

        let service = service_fn(serve_req);
        if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
            error!("server error: {:?}", err);
        };
    }
}
