//! Posts notifications about new change requests in Gerrit
//!
//! # Configuration
//!
//! [`GerritConfig`]
//!
//! ```toml
//! [module."notbot::gerrit".instances.example]
//! instance_url = "https://gerrit.example.org"
//! query = [
//!     [ "status", "open" ],
//!     [ "project", "hscloud" ],
//!     [ "-is", "wip" ],
//! ]
//! limit = 5
//! feed_rooms = [
//!     "#bottest:example.net",
//! ]
//!
//! [module."notbot::gerrit"]
//! feed_interval = 5
//! ```
//!
//! # Usage
//!
//! Worker function [`gerrit_feeds`] provides updates about new CRs to configured rooms.

use crate::prelude::*;

use gerrit_api::{gerrit_decode_json, ChangeInfo};
use reqwest::ClientBuilder;
use tokio::time::{interval, Duration};

/// Configuration of a specific queried Gerrit instance
#[derive(Clone, Debug, Deserialize)]
pub struct GerritInstance {
    /// Base instance URL.
    pub instance_url: String,
    /// Changes query
    #[serde(default = "query")]
    pub query: Vec<(String, String)>,
    /// Limit
    #[serde(default = "limit")]
    pub limit: u64,
    /// Room to post updates to.
    pub feed_rooms: Vec<String>,
}

fn query() -> Vec<(String, String)> {
    vec![("status", "open"), ("project", "hscloud"), ("-is", "wip")]
        .iter()
        .map(|(k, v)| (k.s(), v.s()))
        .collect()
}

fn limit() -> u64 {
    5
}

/// General module configuration
#[derive(Clone, Debug, Deserialize)]
pub struct GerritConfig {
    /// How often the feeds should be checked, in minutes.
    #[serde(default = "feed_interval")]
    pub feed_interval: u64,
    /// Map of Gerrit instances to query and observe
    pub instances: HashMap<String, GerritInstance>,
}

/// Default value for feed checking interval: 5 minutes
pub fn feed_interval() -> u64 {
    5
}

pub(crate) fn workers(mx: &Client, config: &Config) -> anyhow::Result<Vec<WorkerInfo>> {
    info!("registering workers");
    let gerrit_config: GerritConfig = config.typed_module_config(module_path!())?;
    Ok(vec![WorkerInfo::new(
        "gerrit",
        "observes a gerrit instance feed for new changes",
        "gerrit",
        mx.clone(),
        gerrit_config,
        gerrit_feeds,
    )?])
}

/// Process list of changes returned by queries at specified intervals, and
/// post information about new changes to rooms.
pub async fn gerrit_feeds(mx: Client, module_config: GerritConfig) -> anyhow::Result<()> {
    let mut interval = interval(Duration::from_secs(60 * module_config.feed_interval));
    let mut first_loop: HashMap<String, bool> = Default::default();
    let mut change_ids: HashMap<String, Vec<String>> = Default::default();
    let mut known_users: HashMap<(String, u64), String> = Default::default();
    let client = ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;

    for (name, _) in &module_config.instances {
        first_loop.insert(name.to_owned(), true);
        change_ids.insert(name.to_owned(), vec![]);
    }

    loop {
        interval.tick().await;
        for (name, instance) in &module_config.instances {
            let changes_url = format!(
                r#"{base_url}/changes/?q={query}&limit={limit}"#,
                base_url = instance.instance_url,
                limit = instance.limit,
                query = instance
                    .query
                    .iter()
                    .map(|(k, v)| format!("{k}:{v}"))
                    .collect::<Vec<String>>()
                    .join("+")
            );

            let data = match async || -> anyhow::Result<Vec<ChangeInfo>> {
                let text = client.get(changes_url).send().await?.text().await?;
                let data: Vec<ChangeInfo> = gerrit_decode_json(text)?;
                Ok(data)
            }()
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    error!("fetching/decoding changes failed: {e}");
                    continue;
                }
            };

            if first_loop.get(name).unwrap().to_owned() {
                let current_ids: Vec<String> = data.iter().map(|d| d.id.clone()).collect();
                change_ids.insert(name.clone(), current_ids);
                first_loop.insert(name.to_owned(), false);
                continue;
            }

            let known_ids: &Vec<String> = change_ids.get(name).unwrap();
            let mut post_changes = vec![];

            for change in data.clone() {
                if !known_users.contains_key(&(name.to_owned(), change.clone().owner.account_id)) {
                    let user_url = format!(
                        "{instance}/accounts/{user_id}/username",
                        instance = instance.instance_url,
                        user_id = change.owner.account_id,
                    );

                    trace!("user url: {user_url}");
                    let username = match async || -> anyhow::Result<String> {
                        let text = client.get(user_url).send().await?.text().await?;
                        let data: String = gerrit_decode_json(text)?;
                        Ok(data)
                    }()
                    .await
                    {
                        Ok(v) => v,
                        Err(e) => {
                            error!("fetching/decoding username failed: {e}");
                            format!("id:{}", change.owner.account_id)
                        }
                    };

                    trace!("username found: {username}");

                    known_users.insert((name.to_owned(), change.owner.account_id), username);
                }

                if known_ids.contains(&change.id) {
                    continue;
                } else {
                    trace!("added");
                    post_changes.push(change);
                };
            }
            change_ids.insert(name.to_owned(), data.iter().map(|e| e.id.clone()).collect());

            let mut plain_parts: Vec<String> = vec![];
            let mut html_parts: Vec<String> = vec![];

            for change in post_changes {
                // change_url: https://gerrit.hackerspace.pl/c/hscloud/+/2443
                let owner = known_users
                    .get(&(name.to_owned(), change.owner.account_id))
                    .unwrap();
                let project = change.project;
                let subject = change.subject;
                let change_url = format!(
                    "{instance}/c/{project}/+/{change_number}",
                    instance = instance.instance_url,
                    change_number = change.number,
                );

                let plain_message =
                    format!("{owner} has posted a new change to {project}: {subject} {change_url}");

                let html_message = format!(
                    r#"{owner} has posted a new change to {project} <a href="{change_url}">{subject}</a>"#,
                );

                plain_parts.push(plain_message);
                html_parts.push(html_message);
            }

            if plain_parts.is_empty() {
                continue;
            };

            let plain_response = plain_parts.join("\n");
            let html_response = html_parts.join("<br/>");

            for room_name in &instance.feed_rooms {
                let room = match maybe_get_room(&mx, &room_name).await {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                if let Err(e) = room
                    .send(RoomMessageEventContent::text_html(
                        plain_response.clone(),
                        html_response.clone(),
                    ))
                    .await
                {
                    error!("failed to send message: {e}");
                }
            }
        }
    }
}

pub mod gerrit_api {
    //! Helper functions for interacting with Gerrit json APIs
    use anyhow::bail;
    use serde::de;
    use serde_derive::Deserialize;
    use serde_json::Value;
    use std::collections::HashMap;

    /// Strips the `)]}'` prefix from provided text before attempting to deserialize it.
    pub fn gerrit_decode_json<D: de::DeserializeOwned>(text: String) -> anyhow::Result<D> {
        match text.strip_prefix(")]}'") {
            None => bail!("this does not look like a gerrit response"),
            Some(data) => Ok(serde_json::from_str(data)?),
        }
    }

    /// Structure describing information about a change returned from Gerrit
    ///
    /// Written based on [upstream documentation](https://gerrit-review.googlesource.com/Documentation/rest-api-changes.html#change-info)
    /// and responses from a gerrit instance this was developed for.
    ///
    /// Some fields that are documented in the documentation above as required, were missing in the
    /// gerrit instance this module was developed for.
    #[allow(missing_docs)]
    #[derive(Default, Debug, Clone, PartialEq, Deserialize)]
    pub struct ChangeInfo {
        pub id: String,
        // missing for us
        pub triplet_id: Option<String>,
        pub project: String,
        pub branch: String,
        pub topic: Option<String>,
        pub attention_set: Option<HashMap<String, AttentionSetInfo>>,
        pub removed_from_attention_set: Option<HashMap<String, AttentionSetInfo>>,
        pub hashtags: Option<Vec<String>>,
        pub custom_keyed_values: Option<HashMap<String, String>>,
        pub change_id: String,
        pub subject: String,
        pub status: String,
        pub created: String,
        pub updated: String,
        pub submitted: Option<String>,
        pub submitter: Option<String>,
        pub starred: Option<bool>,
        pub reviewed: Option<bool>,
        pub submit_type: Option<String>,
        pub mergeable: Option<String>,
        pub submittable: Option<String>,
        pub insertions: u64,
        pub deletions: u64,
        pub total_comment_count: u64,
        pub unresolved_comment_count: u64,
        #[serde(rename = "_number")]
        pub number: u64,
        // missing for us
        pub virtual_id_number: Option<u64>,
        pub owner: AccountInfo,
        pub actions: Option<ActionInfo>,
        pub requirements: Option<Vec<Requirement>>,
        pub submit_requirements: Option<Vec<SubmitRequirementResultInfo>>,
        // TODO:
        // https://gerrit-review.googlesource.com/Documentation/rest-api-changes.html#label-info
        pub labels: Option<HashMap<String, Value>>,
        pub permitted_labels: Option<Vec<String>>,
        // not even sure how to express that correctly
        // > A map of the removable labels that maps a label name to the map of values and reviewers ( AccountInfo entities) that are allowed to be removed from the change
        // HashMap<String, HashMap<String, AccountInfo>>?
        pub removable_labels: Option<HashMap<String, Value>>,
        pub removable_reviewers: Option<Vec<AccountInfo>>,
        pub reviewers: Option<HashMap<ReviewerState, Vec<AccountInfo>>>,
        pub pending_reviewers: Option<HashMap<ReviewerState, Vec<AccountInfo>>>,
        pub reviewer_updates: Option<Vec<ReviewerUpdateInfo>>,
        pub messages: Option<Vec<ChangeMessageInfo>>,
        // missing for us
        pub current_revision_number: Option<u64>,
        pub current_revision: Option<String>,
        // TODO:
        // https://gerrit-review.googlesource.com/Documentation/rest-api-changes.html#revision-info
        pub revisions: Option<HashMap<String, Value>>,
        pub meta_rev_id: Option<String>,
        pub tracking_ids: Option<Vec<TrackingIdInfo>>,
        #[serde(rename = "_more_changes")]
        pub more_changes: Option<bool>,
        pub problems: Option<Vec<ProblemInfo>>,
        pub is_private: Option<bool>,
        pub work_in_progress: Option<bool>,
        pub has_review_started: Option<bool>,
        pub revert_of: Option<u64>,
        pub submission_id: Option<String>,
        pub cherry_pick_of_change: Option<u64>,
        pub cherry_pick_of_patch_set: Option<u64>,
        pub contains_git_conflicts: Option<bool>,
    }

    #[allow(missing_docs)]
    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct ProblemInfo {
        pub message: String,
        pub status: Option<ProblemInfoStatus>,
        pub outcome: Option<String>,
    }

    #[allow(missing_docs)]
    #[derive(Debug, Clone, Eq, Hash, PartialEq, Deserialize)]
    #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
    pub enum ProblemInfoStatus {
        Fixed,
        FixFailed,
    }

    #[allow(missing_docs)]
    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct TrackingIdInfo {
        pub system: String,
        pub id: String,
    }

    #[allow(missing_docs)]
    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct ChangeMessageInfo {
        pub id: String,
        pub author: Option<AccountInfo>,
        pub real_author: Option<AccountInfo>,
        // timestamp with a known format, we could do something nicer here
        pub date: String,
        pub message: String,
        pub accounts_in_message: Vec<AccountInfo>,
        pub tag: Option<String>,
        #[serde(rename = "_revision_number")]
        pub revision_number: Option<u64>,
    }

    #[allow(missing_docs)]
    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct ReviewerUpdateInfo {
        pub updated: String,
        pub updated_by: AccountInfo,
        pub reviewer: AccountInfo,
        pub state: ReviewerState,
    }

    #[allow(missing_docs)]
    #[derive(Debug, Clone, Eq, Hash, PartialEq, Deserialize)]
    #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
    pub enum ReviewerState {
        Reviewer,
        Cc,
        Removed,
    }

    #[allow(missing_docs)]
    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct SubmitRequirementResultInfo {
        pub name: String,
        pub description: Option<String>,
        pub status: SubmitRequirementStatus,
        pub is_legacy: Option<bool>,
        pub applicability_expression_result: Option<SubmitRequirementExpressionInfo>,
        pub submittability_expression_result: SubmitRequirementExpressionInfo,
        pub override_expression_result: Option<SubmitRequirementExpressionInfo>,
    }

    #[allow(missing_docs)]
    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct SubmitRequirementExpressionInfo {
        pub expression: Option<String>,
        pub fulfilled: bool,
        pub status: SubmitRequirementExpressionInfoStatus,
        pub passing_atoms: Option<Vec<String>>,
        pub failing_atoms: Option<Vec<String>>,
        pub atom_explanations: Option<HashMap<String, String>>,
        pub error_message: Option<String>,
    }

    #[allow(missing_docs)]
    #[derive(Debug, Clone, PartialEq, Deserialize)]
    #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
    pub enum SubmitRequirementExpressionInfoStatus {
        Pass,
        Fail,
        Error,
        NotEvaluated,
    }

    #[allow(missing_docs)]
    #[derive(Debug, Clone, PartialEq, Deserialize)]
    #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
    pub enum SubmitRequirementStatus {
        Satisfied,
        Unsatisfied,
        Overridden,
        NotApplicable,
        Error,
        Forced,
    }

    #[allow(missing_docs)]
    #[derive(Debug, Clone, PartialEq, Deserialize)]
    pub struct Requirement {
        pub status: RequirementStatus,
        pub fallback_text: String,
        #[serde(rename = "type")]
        pub type_field: String,
    }

    #[allow(missing_docs)]
    #[derive(Debug, Clone, PartialEq, Deserialize)]
    #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
    pub enum RequirementStatus {
        Ok,
        NotReady,
        RuleError,
    }

    #[allow(missing_docs)]
    #[derive(Default, Debug, Clone, PartialEq, Deserialize)]
    pub struct AttentionSetInfo {
        pub account: AccountInfo,
        pub last_update: String,
        pub reason: String,
    }

    #[allow(missing_docs)]
    #[derive(Default, Debug, Clone, PartialEq, Deserialize)]
    pub struct AccountInfo {
        #[serde(rename = "_account_id")]
        pub account_id: u64,
    }

    #[allow(missing_docs)]
    #[derive(Default, Debug, Clone, PartialEq, Deserialize)]
    pub struct ActionInfo {
        pub method: Option<String>,
        pub label: Option<String>,
        pub title: Option<String>,
        pub enabled: Option<bool>,
        pub enabled_options: Option<Vec<String>>,
    }
}
