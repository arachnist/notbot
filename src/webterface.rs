use crate::prelude::*;

use crate::alerts::receive_alerts;
use crate::metrics::{serve_metrics, track_metrics};

use axum::{
    error_handling::HandleErrorLayer,
    extract::State,
    http::{StatusCode, Uri},
    middleware, response,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use axum_oidc::{
    error::MiddlewareError, EmptyAdditionalClaims, OidcAccessToken, OidcAuthLayer, OidcClaims,
    OidcLoginLayer,
};
use tokio::net::TcpListener;
use tokio::task::AbortHandle;
use tower::ServiceBuilder;
use tower_http::services::ServeDir;
use tower_sessions::{
    cookie::{time::Duration, SameSite},
    Expiry, MemoryStore, Session, SessionManagerLayer,
};

#[derive(Clone, Deserialize, Debug)]
pub struct ModuleConfig {
    listen_address: String,
    app_url: String,
    userinfo_endpoint: String,
    issuer: String,
    client_id: String,
    client_secret: Option<String>,
}

#[allow(deprecated)]
pub(crate) fn workers() -> Vec<WorkerStarter> {
    vec![(module_path!(), worker_starter)]
}

fn worker_starter(client: &Client, config: &Config) -> anyhow::Result<AbortHandle> {
    let worker = tokio::task::spawn(worker_entrypoint(client.clone(), config.clone()));
    Ok(worker.abort_handle())
}

async fn worker_entrypoint(mx: Client, bot_config: Config) -> anyhow::Result<()> {
    let module_config: ModuleConfig = bot_config.module_config_value(module_path!())?.try_into()?;

    let app_state = WebAppState {
        mx: mx.clone(),
        web_config: module_config.clone(),
        bot_config: bot_config.clone(),
    };

    let session_store = MemoryStore::default();
    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(false)
        .with_same_site(SameSite::Lax)
        .with_expiry(Expiry::OnInactivity(Duration::seconds(3600)));

    let oidc_login_service = ServiceBuilder::new()
        .layer(HandleErrorLayer::new(|e: MiddlewareError| async {
            error!("Failed to handle some error: {e:?}");
            e.into_response()
        }))
        .layer(OidcLoginLayer::<EmptyAdditionalClaims>::new());

    let oidc_auth_service = ServiceBuilder::new()
        .layer(HandleErrorLayer::new(|e: MiddlewareError| async {
            e.into_response()
        }))
        .layer(
            OidcAuthLayer::<EmptyAdditionalClaims>::discover_client(
                Uri::from_str(&module_config.app_url).expect("valid APP_URL"),
                module_config.clone().issuer,
                module_config.clone().client_id,
                module_config.clone().client_secret,
                vec!["profile:read".to_string()],
            )
            .await
            .unwrap(),
        );

    let listen_address = module_config.clone().listen_address;

    let app = Router::new()
        .route("/login", get(login))
        .route("/mx/inviter/invite", get(crate::inviter::web_inviter))
        .layer(oidc_login_service)
        .route("/", get(maybe_authenticated))
        .route("/logout", get(logout))
        .layer(oidc_auth_service)
        .layer(session_layer)
        .nest_service("/static", ServeDir::new("webui/static"))
        .route("/metrics", get(serve_metrics))
        .route("/hook/alerts", post(receive_alerts))
        .route_layer(middleware::from_fn(track_metrics))
        .with_state(app_state);

    let listener = TcpListener::bind(listen_address).await.unwrap();

    axum::serve(listener, app.into_make_service()).await?;

    Ok(())
}

async fn maybe_authenticated(
    claims: Result<OidcClaims<EmptyAdditionalClaims>, axum_oidc::error::ExtractorError>,
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

async fn login(
    token: OidcAccessToken,
    session: Session,
    State(app_state): State<WebAppState>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    async {
        session
            .insert(
                "user.data",
                reqwest::ClientBuilder::new()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()?
                    .get(app_state.web_config.userinfo_endpoint)
                    .bearer_auth(token.0)
                    .send()
                    .await?
                    .json::<OauthUserInfo>()
                    .await?,
            )
            .await?;

        Ok(())
    }
    .await
    .map_err(|err: anyhow::Error| {
        error!("Failed to handle user info: {:?}", err);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to handle user info",
        )
    })?;

    Ok(response::Redirect::to("/"))
}

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

#[derive(Debug, Clone)]
pub struct WebAppState {
    pub mx: Client,
    web_config: ModuleConfig,
    pub bot_config: Config,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct OauthUserInfo {
    pub email: String,
    pub groups: Vec<String>,
    pub name: String,
    pub nickname: String,
    pub preferred_username: String,
    pub sub: String,
}
