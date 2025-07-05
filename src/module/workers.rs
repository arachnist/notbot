//! Bot worker structure definitions.

use super::modules::ModuleInfo;
use super::types::{ConsumerEvent, TriggerType};

use crate::tools::ToStringExt;

use anyhow::bail;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use tracing::{error, warn};

use matrix_sdk::Client;
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;

use askama::Template;

/// Main worker object
///
/// Defines things needed from the worker by the help system
/// Workers are intended to be used by background tasks that act on events outside of matrix, even if the do sometimes interact with matrix.
/// Examples of such tasks include a web interface, or `SpaceAPI` observer.
#[derive(Clone, Debug, Template)]
#[template(
    path = "matrix/help-worker.html",
    blocks = ["formatted", "plain"],
)]
pub struct WorkerInfo {
    /// Worker name
    pub(crate) name: String,
    /// Worker help/description
    pub(crate) help: String,
    /// Keyword to show worker status
    pub(crate) keyword: String,
    /// information about the helper module
    pub(crate) helper_module: ModuleInfo,
    /// Handle to trigger worker stopping
    pub(crate) handle: AbortHandle,
}

impl WorkerInfo {
    /// Builds a new `WokrerInfo` object, and spawns the worker and its associated helper module.
    pub fn new<C, Fut>(
        name: &str,
        help: &str,
        keyword: &str,
        mx: Client,
        config: C,
        worker: impl Fn(Client, C) -> Fut + Send + 'static,
    ) -> Self
    where
        C: Clone + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let handle = tokio::task::spawn(worker(mx, config)).abort_handle();
        let (tx, rx) = mpsc::channel(1);
        let helper_module = ModuleInfo {
            name: name.to_owned(),
            help: help.to_owned(),
            acl: vec![],
            trigger: TriggerType::Keyword(vec![keyword.s()]),
            channel: tx,
            error_prefix: None,
        };
        tokio::task::spawn(Self::status_consumer(rx, handle.clone(), name.to_owned()));

        Self {
            name: name.to_owned(),
            help: help.to_owned(),
            keyword: keyword.to_owned(),
            helper_module,
            handle,
        }
    }

    async fn status_consumer(
        mut rx: mpsc::Receiver<ConsumerEvent>,
        worker_handle: AbortHandle,
        name: String,
    ) -> anyhow::Result<()> {
        loop {
            let Some(event) = rx.recv().await else {
                warn!("{name} channel closed");
                worker_handle.abort();
                bail!("channel closed");
            };

            if let Err(e) = event
                .room
                .send(RoomMessageEventContent::text_plain(format!(
                    "worker running: {}",
                    !worker_handle.is_finished()
                )))
                .await
            {
                error!("error sending worker status response: {e}");
            };
        }
    }
}
