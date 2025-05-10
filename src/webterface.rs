//! Bot web interface
//!
//! Provides a complementary web interface for various bot functions.
//!
//! # Configuration
//! ```toml
//! [module."notbot::webterface"]
//! # String; required; address:port pair for the web interface to bind to
//! listen_address = "100.88.177.77:6543"
//! # String; required; application url used for interacting with SSO
//! app_url = "https://notbot-test.is-a.cat"
//! # String; optional; OIDC token issuer address
//! issuer = "https://sso.hackerspace.pl"
//! # String; required; secret; OIDC client identifier
//! client_id = "…"
//! # String; required; secret; OIDC client secret
//! client_secret = "…"
//!
//! # Usage
//! ```
//! ❯ curl -L notbot.is-a.cat
//! Hello anon!
//! ```
//!
//! # Future
//! Current plan for 0.7.0 is to make endpoint configuration more dynamic, so that loaded bot modules would be able to provide
//! api endpoints and UI snippets.

use crate::prelude::*;

use crate::alerts::receive_alerts;
use crate::metrics::{serve_metrics, track_metrics};

use axum::{
    error_handling::HandleErrorLayer,
    extract::State,
    http::StatusCode,
    middleware, response,
    response::IntoResponse,
    routing::{any, get, post},
    Router,
};
use axum_oidc::{
    error::MiddlewareError, handle_oidc_redirect, AdditionalClaims, OidcAuthLayer, OidcClaims,
    OidcClient, OidcLoginLayer,
};
use openidconnect::ClientSecret;
use tokio::net::TcpListener;
use tokio::time::{sleep, Duration as TokioDuration};
use tower::ServiceBuilder;
use tower_http::services::ServeDir;
use tower_sessions::{
    cookie::{time::Duration, SameSite},
    Expiry, MemoryStore, Session, SessionManagerLayer,
};

#[derive(Clone, Deserialize, Debug)]
struct ModuleConfig {
    listen_address: String,
    app_url: String,
    #[serde(default = "issuer")]
    issuer: String,
    client_id: String,
    client_secret: ClientSecret,
}

fn issuer() -> String {
    "https://sso.hackerspace.pl".s()
}

pub(crate) fn workers(mx: &Client, config: &Config) -> anyhow::Result<Vec<WorkerInfo>> {
    Ok(vec![WorkerInfo::new(
        "webterface",
        "exposes bot web interface",
        "web",
        mx.clone(),
        config.clone(),
        webterface,
    )?])
}

async fn webterface(mx: Client, bot_config: Config) -> anyhow::Result<()> {
    let module_config: ModuleConfig = bot_config.module_config_value(module_path!())?.try_into()?;

    let app_state = WebAppState {
        mx: mx.clone(),
        web_config: module_config.clone(),
        config: bot_config.clone(),
    };

    let session_store = MemoryStore::default();
    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(false)
        .with_same_site(SameSite::Lax)
        .with_expiry(Expiry::OnInactivity(Duration::seconds(3600)));

    let handle_error_layer: HandleErrorLayer<_, ()> =
        HandleErrorLayer::new(|e: MiddlewareError| async {
            error!(error = ?e, "An error occurred in OIDC middleware");
            e.into_response()
        });

    let oidc_login_service = ServiceBuilder::new()
        .layer(handle_error_layer.clone())
        .layer(OidcLoginLayer::<HswawAdditionalClaims>::new());

    let oidc_client = OidcClient::<HswawAdditionalClaims>::builder()
        .with_default_http_client()
        .with_redirect_url(format!("{}/oidc", module_config.app_url).parse()?)
        .with_client_id(module_config.clone().client_id)
        .with_client_secret(module_config.client_secret.secret().clone())
        .add_scope("openid")
        .discover(module_config.issuer.clone())
        .await?
        .build();

    let oidc_auth_service = ServiceBuilder::new()
        .layer(handle_error_layer)
        .layer(OidcAuthLayer::new(oidc_client));

    let listen_address = module_config.clone().listen_address;

    let app = Router::new()
        .route("/login", get(login))
        .route("/mx/inviter/invite", get(crate::inviter::web_inviter))
        .layer(oidc_login_service)
        .route("/oidc", any(handle_oidc_redirect::<HswawAdditionalClaims>))
        .route("/", get(maybe_authenticated))
        .route("/logout", get(logout))
        .layer(oidc_auth_service)
        .layer(session_layer)
        .nest_service("/static", ServeDir::new("webui/static"))
        .route("/metrics", get(serve_metrics))
        .route("/hook/alerts", post(receive_alerts))
        .route_layer(middleware::from_fn(track_metrics))
        .with_state(app_state);

    let mut delay = 1;
    let listener: TcpListener;

    loop {
        let maybe_listener = TcpListener::bind(listen_address.clone()).await;
        match maybe_listener {
            Ok(l) => {
                listener = l;
                break;
            }
            Err(e) => {
                error!("failed setting up tcp listener: {e}; retrying in {delay}s");
                sleep(TokioDuration::from_secs(delay)).await;
                delay += 2;
            }
        };
    }

    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}

async fn maybe_authenticated(
    claims: Result<OidcClaims<HswawAdditionalClaims>, axum_oidc::error::ExtractorError>,
) -> impl IntoResponse {
    if let Ok(claims) = claims {
        format!(
            "Hello {}! You are already logged in from another Handler.",
            claims.subject().as_str()
        )
    } else {
        "Hello anon!".to_string()
    }
}

async fn login() -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    Ok(response::Redirect::to("/"))
}

async fn logout(
    State(app_state): State<WebAppState>,
    session: Session,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    trace!("clearing session");
    session.clear().await;
    trace!("deleting session");
    session.delete().await.map_err(|err| {
        error!("Failed to clear session from store: {:?}", err);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to clear session from store.",
        )
    })?;

    trace!("aaaand we're done");
    Ok(response::Redirect::to(&app_state.web_config.app_url))
}

/// Application state object
#[derive(Debug, Clone)]
pub struct WebAppState {
    /// Matrix client
    pub mx: Client,
    web_config: ModuleConfig,
    /// Full bot configuration
    pub config: Config,
}

/// Additional user information retrieved from oauth userinfo endpoint.
///
/// In addition to claims defined here, some of the data returned from hswaw sso userinfo_endpoint
/// gets mapped to standard claims.
/// These include: sub, name, nickname, preferred_username, email
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HswawAdditionalClaims {
    /// Groups the user belongs to
    pub groups: Option<Vec<String>>,
    /// Primary Matrix UserID of the user
    pub matrix_user: Option<String>,
}

impl openidconnect::AdditionalClaims for HswawAdditionalClaims {}
impl AdditionalClaims for HswawAdditionalClaims {}
