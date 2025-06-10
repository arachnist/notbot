//! Types useful for implementing the web interface.

use crate::prelude::{Config, Deserialize, Serialize, ToStringExt, axum_oidc::AdditionalClaims};

use matrix_sdk::Client;

use axum::{
    extract::FromRequestParts,
    http::{StatusCode, header::AUTHORIZATION, request::Parts},
    response::{IntoResponse, Response},
};
use openidconnect::ClientSecret;

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

/// Application state object
#[derive(Debug, Clone)]
pub struct WebAppState {
    /// Matrix client
    pub mx: Client,
    pub(crate) web_config: ModuleConfig,
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
