use crate::prelude::axum_oidc::OidcClaims;

use super::types::HswawAdditionalClaims;
use askama::Template;

#[derive(Template)]
#[template(path = "web/main.html")]
pub(crate) struct Main {
    pub(crate) claims: Option<OidcClaims<HswawAdditionalClaims>>,
}
