use crate::{Config, WorkerStarter, WORKERS};
use anyhow::{anyhow, Context};
use std::convert::Infallible;

use tracing::error;

use matrix_sdk::Client;

use linkme::distributed_slice;
use serde_derive::{Deserialize, Serialize};

use axum::{
    extract::{FromRef, FromRequestParts, OptionalFromRequestParts, Query, State},
    response::{IntoResponse, Redirect, Response},
    routing::get,
    Router,
};
use http::{request::Parts, StatusCode};
use oauth2::{
    basic::{BasicClient, BasicErrorResponseType, BasicTokenType},
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EmptyExtraTokenFields,
    EndpointNotSet, EndpointSet, RedirectUrl, RevocationErrorResponseType, Scope,
    StandardErrorResponse, StandardRevocableToken, StandardTokenIntrospectionResponse,
    StandardTokenResponse, TokenResponse, TokenUrl,
};
use tokio::net::TcpListener;
use tokio::task::AbortHandle;
use tower_sessions::{cookie::time::Duration, Expiry, MemoryStore, Session, SessionManagerLayer};

static CSRF_TOKEN: &str = "csrf_token";

type O2Client = oauth2::Client<
    StandardErrorResponse<BasicErrorResponseType>,
    StandardTokenResponse<EmptyExtraTokenFields, BasicTokenType>,
    StandardTokenIntrospectionResponse<EmptyExtraTokenFields, BasicTokenType>,
    StandardRevocableToken,
    StandardErrorResponse<RevocationErrorResponseType>,
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointSet,
>;

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
    redirect_url: String,
}

#[derive(Clone)]
struct AppState {
    store: MemoryStore,
    oauth_client: O2Client,
    http_client: reqwest::Client,
    matrix_client: Client,
    config: ModuleConfig,
}

impl FromRef<AppState> for MemoryStore {
    fn from_ref(state: &AppState) -> Self {
        state.store.clone()
    }
}

impl FromRef<AppState> for O2Client {
    fn from_ref(state: &AppState) -> Self {
        state.oauth_client.clone()
    }
}

impl FromRef<AppState> for reqwest::Client {
    fn from_ref(state: &AppState) -> Self {
        state.http_client.clone()
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
    let session_store = MemoryStore::default();
    let session_layer = SessionManagerLayer::new(session_store.clone())
        .with_secure(false)
        .with_expiry(Expiry::OnInactivity(Duration::new(60 * 60, 0)));

    let oauth_client = oauth_client(config.clone())?;
    let http_client = reqwest::Client::new();

    let app_state = AppState {
        store: session_store,
        oauth_client,
        http_client,
        matrix_client: mx,
        config: config.clone(),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/auth/login", get(oauth_auth))
        .route("/auth/authorized", get(login_authorized))
        .route("/protected", get(protected))
        .route("/logout", get(logout))
        .layer(session_layer)
        .with_state(app_state);

    let listener = TcpListener::bind(config.listen_address).await.unwrap();

    axum::serve(listener, app.into_make_service())
        .await
        .unwrap();

    Ok(())
}

fn oauth_client(config: ModuleConfig) -> anyhow::Result<O2Client> {
    Ok(BasicClient::new(ClientId::new(config.clone().client_id))
        .set_client_secret(ClientSecret::new(config.clone().client_secret))
        .set_auth_uri(AuthUrl::new(config.clone().auth_url)?)
        .set_token_uri(TokenUrl::new(config.clone().token_url)?)
        .set_redirect_uri(RedirectUrl::new(config.clone().redirect_url)?))
}

#[derive(Debug)]
struct User {
    session: Session,
    user_data: UserData,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
struct UserData {
    sub: String,
    email: String,
}

impl User {
    const USER_DATA_KEY: &'static str = "user.data";

    fn sub(&self) -> String {
        self.user_data.sub.clone()
    }

    fn email(&self) -> String {
        self.user_data.email.clone()
    }

    async fn update_session(session: &Session, user_data: &UserData) {
        session
            .insert(Self::USER_DATA_KEY, user_data.clone())
            .await
            .unwrap()
    }
}

async fn index(user: Option<User>) -> impl IntoResponse {
    match user {
        Some(u) => format!(
            "Hey {}! You're logged in!\nYou may now access `/protected`.\nLog out with `/logout`.",
            u.sub()
        ),
        None => "You're not logged in.\nVisit `/auth/oauth2` to do so.".to_string(),
    }
}

async fn oauth_auth(
    State(config): State<ModuleConfig>,
    session: Session,
) -> Result<impl IntoResponse, AppError> {
    let client = BasicClient::new(ClientId::new(config.clone().client_id))
        .set_client_secret(ClientSecret::new(config.clone().client_secret))
        .set_auth_uri(AuthUrl::new(config.clone().auth_url)?)
        .set_token_uri(TokenUrl::new(config.clone().token_url)?)
        .set_redirect_uri(RedirectUrl::new(config.clone().redirect_url)?);

    let (auth_url, csrf_token) = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new("identify".to_string()))
        .url();

    // Create session to store csrf_token
    session
        .insert(CSRF_TOKEN, &csrf_token)
        .await
        .context("failed in inserting CSRF token into session")?;

    session.save().await?;

    Ok(Redirect::to(auth_url.as_ref()))
}

// Valid user session required. If there is none, redirect to the auth page
async fn protected(user: User) -> impl IntoResponse {
    format!("Welcome to the protected area :)\nHere's your info:\n{user:?}")
}

async fn logout(session: Session) -> Result<impl IntoResponse, AppError> {
    if let Err(e) = session.delete().await {
        error!("couldn't delete session: {e}");
    };

    Ok(Redirect::to("/"))
}

struct AuthRedirect;

impl IntoResponse for AuthRedirect {
    fn into_response(self) -> Response {
        Redirect::temporary("/auth/login").into_response()
    }
}

impl<S> FromRequestParts<S> for User
where
    S: Send + Sync,
{
    type Rejection = AuthRedirect;

    async fn from_request_parts(req: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let session = match Session::from_request_parts(req, state).await {
            Ok(s) => s,
            Err(e) => {
                error!("couldn't get session: {e:?}");
                return Err(AuthRedirect);
            }
        };

        let user_data: UserData = session
            .get(Self::USER_DATA_KEY)
            .await
            .unwrap()
            .unwrap_or_default();

        Self::update_session(&session, &user_data).await;

        Ok(Self { session, user_data })
    }
}

impl<S> OptionalFromRequestParts<S> for User
where
    MemoryStore: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> Result<Option<Self>, Self::Rejection> {
        match <User as FromRequestParts<S>>::from_request_parts(parts, state).await {
            Ok(res) => Ok(Some(res)),
            Err(AuthRedirect) => Ok(None),
        }
    }
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AuthRequest {
    code: String,
    state: String,
}

async fn csrf_token_validation_workflow(
    auth_request: &AuthRequest,
    session: &Session,
) -> anyhow::Result<()> {
    let Ok(Some(stored_csrf_token)) = session.get::<CsrfToken>(CSRF_TOKEN).await else {
        return Err(anyhow!("CSRF token not found"));
    };

    session.delete().await?;

    // Validate CSRF token is the same as the one in the auth request
    if *stored_csrf_token.secret() != auth_request.state {
        return Err(anyhow!("CSRF token mismatch").into());
    }

    Ok(())
}

async fn login_authorized(
    Query(query): Query<AuthRequest>,
    session: Session,
    State(oauth_client): State<O2Client>,
    State(http_client): State<reqwest::Client>,
    State(config): State<ModuleConfig>,
) -> Result<impl IntoResponse, AppError> {
    csrf_token_validation_workflow(&query, &session).await?;

    let token = oauth_client
        .exchange_code(AuthorizationCode::new(query.code.clone()))
        .request_async(&http_client)
        .await?;

    let user_data: UserData = http_client
        .get(config.userinfo_endpoint)
        .bearer_auth(token.access_token().secret())
        .send()
        .await?
        .json::<UserData>()
        .await?;

    User::update_session(&session, &user_data).await;

    Ok(Redirect::to("/"))
}
