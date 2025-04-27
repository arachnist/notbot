use std::str::FromStr;

use crate::{
    Config, WorkerStarter, WORKERS
};

use matrix_sdk::Client;

use linkme::distributed_slice;
use serde_derive::Deserialize;

use tokio::task::AbortHandle;
use axum::{
    error_handling::HandleErrorLayer, http::Uri, response::IntoResponse, routing::get, Router,
};
use axum_oidc::{
    error::MiddlewareError, EmptyAdditionalClaims, OidcAuthLayer, OidcClaims, OidcLoginLayer,
    OidcRpInitiatedLogout,
};
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_sessions::{
    cookie::{time::Duration, SameSite},
    Expiry, MemoryStore, SessionManagerLayer,
};

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct ModuleConfig {
    listen_address: String,
    app_url: String,
    issuer: String,
    client_id: String,
    client_secret: Option<String>,
    userinfo_endpoint: String,
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
                module_config.issuer,
                module_config.client_id,
                module_config.client_secret,
                vec![]
            )
            .await
            .unwrap(),
        );

    let app = Router::new()
        .route("/logout", get(logout))
        .layer(oidc_login_service)
        .route("/blabla", get(maybe_authenticated))
        .layer(oidc_auth_service)
        .layer(session_layer);

    let listener = TcpListener::bind(module_config.listen_address).await.unwrap();

    axum::serve(listener, app.into_make_service())
        .await.unwrap();
    
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

async fn logout(logout: OidcRpInitiatedLogout) -> impl IntoResponse {
    logout.with_post_logout_redirect(Uri::from_static("https://notbot.is-a.cat/blabla"))
}
