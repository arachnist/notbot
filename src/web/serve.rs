//! Basic functions for serving the web interface.

use super::metrics::{serve_metrics, track_metrics};
use super::templates;
use super::types::{HswawAdditionalClaims, ModuleConfig, WebAppState, WebError};

use crate::alerts::grafana;

use crate::prelude::Config;

use crate::prelude::{Duration as TokioDuration, axum_oidc, error, trace};

use matrix_sdk::Client;

use askama::Template;
use axum::{
    Router,
    error_handling::HandleErrorLayer,
    extract::State,
    http::StatusCode,
    middleware,
    response::{Html, IntoResponse, Redirect},
    routing::{any, get, post},
};
use axum_oidc::{
    OidcAuthLayer, OidcClaims, OidcClient, OidcLoginLayer, error::MiddlewareError,
    handle_oidc_redirect,
};
use tokio::{net::TcpListener, time::sleep};
use tower::ServiceBuilder;
use tower_http::services::ServeDir;
use tower_sessions::{
    Expiry, MemoryStore, Session, SessionManagerLayer,
    cookie::{SameSite, time::Duration},
};

/// Sets up an OIDC client, auth and login layers, session store, some - for the time being - hardcoded routes, listens on the configured socket, and starts serving requests.
/// # Errors
/// Will return `Err` if:
/// * configuration is malformed
/// * building oidc client fails
pub async fn serve(mx: Client, bot_config: Config) -> anyhow::Result<()> {
    trace!("starting the web server");

    let module_config: ModuleConfig = bot_config.typed_module_config("notbot::web")?;

    let app_state = WebAppState {
        mx: mx.clone(),
        web_config: module_config.clone(),
        config: bot_config.clone(),
    };

    trace!("configuring session management");
    let session_store = MemoryStore::default();
    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(false)
        .with_same_site(SameSite::Lax)
        .with_expiry(Expiry::OnInactivity(Duration::seconds(3600)));

    trace!("configuring error management");
    let handle_error_layer: HandleErrorLayer<_, ()> =
        HandleErrorLayer::new(|e: MiddlewareError| async {
            error!(error = ?e, "An error occurred in OIDC middleware");
            e.into_response()
        });

    trace!("configuring oidc client");
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

    trace!("configuring http router");
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
        .route("/hook/alerts", post(grafana::receive_alerts))
        .route_layer(middleware::from_fn(track_metrics))
        .with_state(app_state);

    let mut delay = 1;
    let listener: TcpListener;
    let listen_address = module_config.clone().listen_address;

    trace!("attempting to start tcp listener");
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

    trace!("starting web listener");
    axum::serve(listener, app.into_make_service()).await.map_err(|e| e.into())
}

/// Temporary main response for the web interface. Responds with different strings, depending on whether or not the user is authenthicated.
/// # Errors
/// Will return `Err` if rendering the templates fails. Shouldn't happen, unless OIDC provider responds with malformed userinfo.
#[axum::debug_handler]
pub async fn maybe_authenticated(
    claims: Result<OidcClaims<HswawAdditionalClaims>, axum_oidc::error::ExtractorError>,
) -> Result<Html<String>, WebError> {
    let main = templates::Main {
        claims: claims.ok(),
    };
    Ok(Html(main.render()?))
}

/// Dummy handler for `/login` endpoint, to make unauthenthicated users go through OIDC flow.
pub async fn login() -> impl IntoResponse {
    Redirect::to("/")
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
    Ok(Redirect::to(&app_state.web_config.app_url))
}
