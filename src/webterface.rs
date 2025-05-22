//! Bot web interface
//!
//! Provides a complementary web interface for various bot functions.
//!
//! [`ModuleConfig`]
//!
//! # Configuration
//! ```toml
//! [module."notbot::webterface"]
//! listen_address = "100.88.177.77:6543"
//! app_url = "https://notbot-test.is-a.cat"
//! issuer = "https://sso.hackerspace.pl"
//! client_id = "…"
//! client_secret = "…"
//! ```
//!
//! # Usage
//!
//! Web interface entrypoint: [`webterface`]
//!
//! Sets up an OIDC client, auth and login layers, session store, some - for the time being - hardcoded routes, listens on the configured socket, and starts serving requests.
//! Currently handled endpoints:
//! * `/login` - [`login`] - static known url responding, after auth, with redirect to `/`, to force users to go through OIDC flow.
//! * `/mx/inviter/invite` - [`crate::inviter::web_inviter`] - Proof-of-concept for the self-service matrix Room inviter.
//! * `/oidc` - [`handle_oidc_redirect`] - Handler for OIDC redirects, requesting additional hswaw-specific claims (account properties known by the issuer).
//! * `/` - [`maybe_authenticated`] - Main endpoint if it can be called that. Responds with different text for authenthicated users.
//! * `/static` - [`ServeDir`] - serving files from `webui/static`, if there are any.
//! * `/metrics` - [`serve_metrics`] - serves prometheus metrics exported by the bot.
//! * `/hook/alerts` - [`receive_alerts`] - endpoint for receiving webhook requests from grafana instances configured in [`crate::alerts`] module
//!
//! ```text
//! ❯ curl https://notbot.is-a.cat
//! <!DOCTYPE html>
//! <html>
//! ```
//!
//! # Future
//!
//! Current plan for 0.7.0 is to make endpoint configuration more dynamic, so that loaded bot modules would be able to provide
//! api endpoints and UI snippets.
//!
//! Persistence for sessions maybe?

use crate::prelude::*;

use crate::alerts::receive_alerts;
use crate::metrics::{serve_metrics, track_metrics};

use askama;
use askama::Template;
use axum::{
    Router,
    error_handling::HandleErrorLayer,
    extract::State,
    http::{StatusCode, header::AUTHORIZATION, request::Parts},
    middleware, response,
    response::{Html, IntoResponse, Response},
    routing::{any, get, post},
};
use axum_core::extract::FromRequestParts;
use axum_oidc::{
    AdditionalClaims, OidcAuthLayer, OidcClaims, OidcClient, OidcLoginLayer,
    error::MiddlewareError, handle_oidc_redirect,
};
use openidconnect::ClientSecret;
use tokio::net::TcpListener;
use tokio::time::{Duration as TokioDuration, sleep};
use tower::ServiceBuilder;
use tower_http::services::ServeDir;
use tower_sessions::{
    Expiry, MemoryStore, Session, SessionManagerLayer,
    cookie::{SameSite, time::Duration},
};

/// Web interface configuration
#[derive(Clone, Deserialize, Debug)]
pub struct ModuleConfig {
    /// Address to listen on. Passed directly to [`TcpListener::bind`]
    pub listen_address: String,
    /// App url, used for constructing redirects for OIDC purposes.
    pub app_url: String,
    /// OIDC token issuer address.
    #[serde(default = "issuer")]
    pub issuer: String,
    /// Unique OIDC client identifier for the bot instance.
    pub client_id: String,
    /// OIDC client secret token.
    pub client_secret: ClientSecret,
}

fn issuer() -> String {
    "https://sso.hackerspace.pl".s()
}

#[allow(clippy::unnecessary_wraps, reason = "required by caller")]
pub(crate) fn workers(mx: &Client, config: &Config) -> anyhow::Result<Vec<WorkerInfo>> {
    Ok(vec![WorkerInfo::new(
        "webterface",
        "exposes bot web interface",
        "web",
        mx.clone(),
        config.clone(),
        webterface,
    )])
}

/// Sets up an OIDC client, auth and login layers, session store, some - for the time being - hardcoded routes, listens on the configured socket, and starts serving requests.
/// # Errors
/// Will return `Err` if:
/// * configuration is malformed
/// * building oidc client fails
pub async fn webterface(mx: Client, bot_config: Config) -> anyhow::Result<()> {
    let module_config: ModuleConfig = bot_config.typed_module_config(module_path!())?;

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

/// Temporary main response for the web interface. Responds with different strings, depending on whether or not the user is authenthicated.
/// # Errors
/// Will return `Err` if rendering the templates fails. Shouldn't happen, unless OIDC provider responds with malformed userinfo.
#[axum::debug_handler]
pub async fn maybe_authenticated(
    claims: Result<OidcClaims<HswawAdditionalClaims>, axum_oidc::error::ExtractorError>,
) -> Result<Html<String>, WebError> {
    if let Ok(claims) = claims {
        let main = templates::Main { claims };

        Ok(Html(main.render()?))
    } else {
        Ok(Html(templates::MainAnon {}.render()?))
    }
}

/// Dummy handler for `/login` endpoint, to make unauthenthicated users go through OIDC flow.
pub async fn login() -> impl IntoResponse {
    response::Redirect::to("/")
}

/// Handler for the `/logout` endpoint. Removes local app/user specific session information.
/// # Errors
/// Will return error if deleting session in the local session store fails.
pub async fn logout(
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
/// In addition to claims defined here, some of the data returned from hswaw sso [userinfo endpoint](https://sso.hackerspace.pl/api/1/userinfo)
/// gets mapped to standard claims.
/// These include: sub, name, nickname, preferred username, email
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HswawAdditionalClaims {
    /// Groups the user belongs to
    pub groups: Option<Vec<String>>,
    /// Primary Matrix User ID of the user
    pub matrix_user: Option<String>,
}

impl openidconnect::AdditionalClaims for HswawAdditionalClaims {}
impl AdditionalClaims for HswawAdditionalClaims {}

/// Simple extractor for Bearer auth.
///
/// Will check if `Authorization: Bearer …` header is present, and return the contents (after `Bearer`)
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

impl AuthBearer {
    const ERROR_CODE: StatusCode = StatusCode::FORBIDDEN;

    fn from_header(contents: &str) -> Self {
        Self(contents.to_string())
    }

    fn decode_request_parts(req: &Parts) -> Result<Self, (StatusCode, &'static str)> {
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
            Some(("Bearer", contents)) => Ok(Self::from_header(contents)),
            _ => Err((Self::ERROR_CODE, "Authorization header invalid")),
        }
    }
}

/// Error responses from http web interface
#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum WebError {
    /// not found
    NotFound,
    /// could not render template
    Render(#[from] askama::Error),
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        match &self {
            Self::NotFound => (StatusCode::NOT_FOUND, "content not found").into_response(),
            Self::Render(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "something went wrong").into_response()
            }
        }
    }
}

mod templates {
    use super::HswawAdditionalClaims;
    use super::OidcClaims;
    use askama::Template;

    #[derive(Template)]
    #[template(path = "web/main-anon.html")]
    pub struct MainAnon {}

    #[derive(Template)]
    #[template(path = "web/main.html")]
    pub struct Main {
        pub claims: OidcClaims<HswawAdditionalClaims>,
    }
}
