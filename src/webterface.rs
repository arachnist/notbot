use std::str::FromStr;
use tracing::{error, trace};

use crate::{
    metrics::{serve_metrics, track_metrics},
    Config, WorkerStarter, WORKERS,
};

use matrix_sdk::Client;

use linkme::distributed_slice;
use serde_derive::{Deserialize, Serialize};

use axum::{
    error_handling::HandleErrorLayer,
    extract::State,
    http::{StatusCode, Uri},
    middleware, response,
    response::IntoResponse,
    routing::get,
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

#[derive(Clone, Deserialize)]
pub struct ModuleConfig {
    listen_address: String,
    app_url: String,
    userinfo_endpoint: String,
    issuer: String,
    client_id: String,
    client_secret: Option<String>,
}

#[distributed_slice(WORKERS)]
static WORKER_STARTER: WorkerStarter = (module_path!(), worker_starter);

fn worker_starter(client: &Client, config: &Config) -> anyhow::Result<AbortHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
    let worker = tokio::task::spawn(worker_entrypoint(client.clone(), module_config));
    Ok(worker.abort_handle())
}

async fn worker_entrypoint(_mx: Client, module_config: ModuleConfig) -> anyhow::Result<()> {
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
        .layer(oidc_login_service)
        .route("/", get(maybe_authenticated))
        .route("/logout", get(logout))
        .layer(oidc_auth_service)
        .layer(session_layer)
        .nest_service("/static", ServeDir::new("webui/static"))
        .route("/metrics", get(serve_metrics))
        .route_layer(middleware::from_fn(track_metrics))
        .with_state(module_config.clone());

    let listener = TcpListener::bind(listen_address).await.unwrap();

    axum::serve(listener, app.into_make_service()).await?;

    Ok(())
}

#[axum::debug_handler]
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
    State(config): State<ModuleConfig>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let http_client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|err| {
            error!(
                "Failed to build http client for fetching extra user info: {:?}",
                err
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to build http client.",
            )
        })?;

    let userinfo: UserInfo = match http_client
        .get(config.userinfo_endpoint)
        .bearer_auth(token.0)
        .send()
        .await
    {
        Err(e) => {
            error!("Failed to fetch user info: {:?}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch user info.",
            ));
        }
        Ok(r) => r.json().await.map_err(|err| {
            error!("Failed to decode user info.: {:?}", err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to decode user info..",
            )
        })?,
    };

    session.insert("user.data", userinfo).await.map_err(|err| {
        error!("Failed to store user info: {:?}", err);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to store user info.",
        )
    })?;

    Ok(response::Redirect::to("/"))
}

pub async fn logout(
    State(config): State<ModuleConfig>,
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
    Ok(response::Redirect::to(&config.app_url))
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserInfo {
    pub email: String,
    pub groups: Vec<String>,
    pub name: String,
    pub nickname: String,
    pub preferred_username: String,
    pub sub: String,
}
