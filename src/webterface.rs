use anyhow::Context;
use crate::{
    Config, WorkerStarter, WORKERS
};

use matrix_sdk::Client;

use linkme::distributed_slice;
use serde_derive::{Serialize, Deserialize};

use async_session::{MemoryStore, Session, SessionStore};
use tokio::task::AbortHandle;
use axum::{
        response::{IntoResponse, Redirect, Response},
    extract::{FromRef, FromRequestParts, OptionalFromRequestParts, Query, State}, error_handling::HandleErrorLayer, routing::get, Router,
        http::{header::SET_COOKIE, HeaderMap, Uri},
};
use http::{header, request::Parts, StatusCode};
use tokio::net::TcpListener;
use oauth2::{
    basic::BasicClient, AuthUrl, AuthorizationCode, ClientId,
    ClientSecret, CsrfToken, RedirectUrl, Scope, TokenResponse, TokenUrl,
    EndpointNotSet, EndpointMaybeSet, EndpointSet,
};

static COOKIE_NAME: &str = "SESSION";
static CSRF_TOKEN: &str = "csrf_token";

#[derive(Debug)]
struct AppError(anyhow::Error);

// Tell axum how to convert `AppError` into a response.
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        tracing::error!("Application error: {:#}", self.0);

        (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong").into_response()
    }
}

// This enables using `?` on functions that return `Result<_, anyhow::Error>` to turn them into
// `Result<_, AppError>`. That way you don't need to do that manually.
impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct ModuleConfig {
    listen_address: String,
    app_url: String,
    issuer: String,
    client_id: String,
    client_secret: String,
    userinfo_endpoint: String,
    auth_url: String,
    token_url: String,
    redirect_url: String
}

#[derive(Clone)]
struct AppState {
    store: MemoryStore,
    oauth_client: BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>,
    matrix_client: Client,
    config: ModuleConfig,
}

impl FromRef<AppState> for MemoryStore {
    fn from_ref(state: &AppState) -> Self {
        state.store.clone()
    }
}

impl FromRef<AppState> for BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet> {
    fn from_ref(state: &AppState) -> Self {
        state.oauth_client.clone()
    }
}

impl FromRef<AppState> for Client {
    fn from_ref(state: &AppState) -> Self {
        state.matrix_client.clone()
    }
}

impl FromRef<AppState> for ModuleConfig {
    fn from_ref(state: &AppState) -> Self {
        state.config.clone()
    }
}

#[distributed_slice(WORKERS)]
static WORKER_STARTER: WorkerStarter = (module_path!(), worker_starter);

fn worker_starter(client: &Client, config: &Config) -> anyhow::Result<AbortHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
    let worker = tokio::task::spawn(worker_entrypoint(client.clone(), module_config));
    Ok(worker.abort_handle())
}

async fn worker_entrypoint(mx: Client, config: ModuleConfig) -> anyhow::Result<()> {
    let store = MemoryStore::new();
    let oauth_client = BasicClient::new(ClientId::new(config.clone().client_id))
        .set_client_secret(ClientSecret::new(config.clone().client_secret))
        .set_auth_uri(AuthUrl::new(config.clone().auth_url)?)
        .set_token_uri(TokenUrl::new(config.clone().token_url)?)
        .set_redirect_uri(RedirectUrl::new(config.clone().redirect_url)?);

    let app_state = AppState {
        store,
        oauth_client,
        matrix_client: mx,
        config: config.clone(),
    };

    let app = Router::new() /*
        .route("/logout", get(logout))
        .layer(oidc_login_service)
        .route("/blabla", get(maybe_authenticated))
        .layer(oidc_auth_service)
        .layer(session_layer) */;

    let listener = TcpListener::bind(config.listen_address).await.unwrap();

    axum::serve(listener, app.into_make_service())
        .await.unwrap();
    
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct User {
    sub: String,
    email: String,
}

async fn index(user: Option<User>) -> impl IntoResponse {
    match user {
        Some(u) => format!(
            "Hey {}! You're logged in!\nYou may now access `/protected`.\nLog out with `/logout`.",
            u.sub
        ),
        None => "You're not logged in.\nVisit `/auth/oauth2` to do so.".to_string(),
    }
}

