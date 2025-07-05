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

pub mod metrics;
pub mod serve;
mod templates;
pub mod types;

use serve::serve;
use types::ModuleConfig;

use crate::prelude::trace;
use crate::prelude::{Config, WorkerInfo};

use matrix_sdk::Client;

#[allow(clippy::unnecessary_wraps, reason = "required by caller")]
pub(crate) fn workers(mx: &Client, config: &Config) -> anyhow::Result<Vec<WorkerInfo>> {
    let module_config: ModuleConfig = config.typed_module_config(module_path!())?;
    let help_string = format!("exposes bot web interface at {}", module_config.app_url);

    trace!("initializing web interface");

    Ok(vec![WorkerInfo::new(
        "webterface",
        help_string.as_str(),
        "web",
        mx.clone(),
        config.clone(),
        serve,
    )])
}
