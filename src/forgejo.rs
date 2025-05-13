//! Query Frogejo API for latest, and oldest open issues pull request, and post notifications about new events.
//!
//! # Configuration
//!
//! [`ForgejoConfig`]
//!
//! ```toml
//! [module."notbot::forgejo".instances.example]
//! instance_url = "https://code.example.org"
//! token_name = "notbot-test"
//! token_secret = "…"
//! organizations = [ "orga" ]
//! feed_rooms = [
//!     "#bottest:example.net",
//!     "#infra:example.org",
//! ]
//! events = [
//!     "create_pull_request",
//!     "merge_pull_request",
//!     "pull_request_ready_for_review",
//!     "approve_pull_request",
//!     "reject_pull_request",
//!     "create_issue",
//!     "close_issue",
//!     "reopen_pull_request",
//! ]
//!
//! [module."notbot::forgejo".rooms]
//! "default" = "example"
//!
//! [module."notbot::forgejo"]
//! feed_interval = 5
//! ```
//!
//! # Usage
//!
//! Keywords:
//! * `pr-new`, `prnew`, `pr`, `p` - [`pr_new`] - show latest open pull requests
//! * `pr-old`, `prold` - [`pr_old`] - show oldest open pull requests
//! * `issue-new`, `issuenew`, `issue`, `i` - [`issue_new`] - show latest issues
//! * `issue-old`, `issueold` - [`issue_old`] - show oldest open issues
//!
//! Passive feed updates: [`forgejo_feeds`]
//! Provides updates about configured events to configured rooms.

use crate::prelude::*;

use std::fmt::Debug;

use tokio::time::{interval, Duration};

use forgejo_api::structs::ActivityOpType;
use forgejo_api::{Auth, Forgejo};

/// Turns the ActivityOpType into a human readable string, admittedly somewhat naively.
pub fn verb_from_activity_type(act: ActivityOpType) -> String {
    use ActivityOpType::*;
    match act {
        CreateRepo => "created repository",
        RenameRepo => "renamed repository",
        StarRepo => "starred",
        WatchRepo => "started watching",
        CommitRepo => "commited to",
        CreateIssue => "created issue in",
        CreatePullRequest => "created pull request in",
        TransferRepo => "transferred",
        PushTag => "pushed tag in",
        CommentIssue => "commented on issue in",
        MergePullRequest => "merged pull request in",
        CloseIssue => "closed issue in",
        ReopenIssue => "reopened issue in",
        ClosePullRequest => "closed pull request in",
        ReopenPullRequest => "reopened pull request in",
        DeleteTag => "deleted tag in",
        DeleteBranch => "deleted branch in",
        MirrorSyncPush => "mirror operation has pushed sync to",
        MirrorSyncCreate => "mirror has created",
        MirrorSyncDelete => "mirror has deleted",
        ApprovePullRequest => "has approved pull request in",
        RejectPullRequest => "has rejected pull request in",
        CommentPull => "has commented on pull request in",
        PublishRelease => "has published release for",
        PullReviewDismissed => "has dismissed pull review in",
        PullRequestReadyForReview => "has marked pull request as ready for review in",
        AutoMergePullRequest => "has automatically merged pull request in",
        // _ => todo!(), rustc doesn't like the possibility that forgejo might implement more features ;)
    }
    .s()
}

/// Configuration of a forgejo instance.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ForgejoInstance {
    /// Base instance URL.
    pub instance_url: String,
    /// Configured token name.
    pub token_name: String,
    /// Generated token secret.
    pub token_secret: String,
    /// Organization to observe changes in.
    pub organizations: Vec<String>,
    /// Room to post updates to.
    pub feed_rooms: Vec<String>,
    /// Event types to post about
    #[serde(default = "forgejo_events_default")]
    pub events: Vec<ActivityOpType>,
}

/// Default event types to post notifications for
pub fn forgejo_events_default() -> Vec<ActivityOpType> {
    use ActivityOpType::*;
    vec![
        CreatePullRequest,
        MergePullRequest,
        PullRequestReadyForReview,
        ApprovePullRequest,
        RejectPullRequest,
        ReopenPullRequest,
        CreateIssue,
        CloseIssue,
        ReopenIssue,
        CreateRepo,
    ]
}

/// General module configuration
#[derive(Clone, Debug, Deserialize)]
pub struct ForgejoConfig {
    /// Instance to act upon in a given room. Special key "default" signifies the default instance if none is configured for a given room.
    pub rooms: HashMap<String, String>,
    /// How often the feeds should be checked, in minutes.
    #[serde(default = "feed_interval")]
    pub feed_interval: u64,
    /// How many issues/PRs should be returned on active queries
    #[serde(default = "objects_count")]
    pub objects_count: u64,
    /// Map of Forgejo instances to query and observe
    pub instances: HashMap<String, ForgejoInstance>,
    /// Keywords for displaying list of latest open pull requests.
    #[serde(default = "keywords_pr_new")]
    pub keywords_pr_new: Vec<String>,
    /// Keywords for displaying list of oldest open pull requests.
    #[serde(default = "keywords_pr_old")]
    pub keywords_pr_old: Vec<String>,
    /// Keywords for displaying list of latest open issues.
    #[serde(default = "keywords_issue_new")]
    pub keywords_issue_new: Vec<String>,
    /// Keywords for displaying list of oldest open issues.
    #[serde(default = "keywords_issue_old")]
    pub keywords_issue_old: Vec<String>,
}

/// Default value for feed checking interval: 5 minutes
pub fn feed_interval() -> u64 {
    5
}

/// Default value for returned objects count: 3
pub fn objects_count() -> u64 {
    3
}

/// Default keywords for displaying list of latest open pull requests: pr-new, prnew, pr, p
pub fn keywords_pr_new() -> Vec<String> {
    vec!["pr-new".s(), "prnew".s(), "pr".s(), "p".s()]
}

/// Default keywords for displaying list of oldest open pull requests: pr-old, prold
pub fn keywords_pr_old() -> Vec<String> {
    vec!["pr-old".s(), "prold".s()]
}

/// Default keywords for displaying list of latest open issues: issue-new, issuenew, issue, i
pub fn keywords_issue_new() -> Vec<String> {
    vec!["issue-new".s(), "issuenew".s(), "issue".s(), "i".s()]
}

/// Default keywords for displaying list of oldest open issues: issue-old, issueold
pub fn keywords_issue_old() -> Vec<String> {
    vec!["issue-old".s(), "issueold".s()]
}

pub(crate) fn starter(_: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering modules");
    let forgejo_config: ForgejoConfig = config.typed_module_config(module_path!())?;

    Ok(vec![
        ModuleInfo::new(
            "pr_new",
            "display list of latest open pull requests",
            vec![],
            TriggerType::Keyword(forgejo_config.keywords_pr_new.clone()),
            Some("forgejo communications error"),
            forgejo_config.clone(),
            pr_new,
        ),
        ModuleInfo::new(
            "pr_old",
            "display list of oldest open pull requests",
            vec![],
            TriggerType::Keyword(forgejo_config.keywords_pr_old.clone()),
            Some("forgejo communications error"),
            forgejo_config.clone(),
            pr_old,
        ),
        ModuleInfo::new(
            "issue_new",
            "display list of latest open issues",
            vec![],
            TriggerType::Keyword(forgejo_config.keywords_issue_new.clone()),
            Some("forgejo communications error"),
            forgejo_config.clone(),
            issue_new,
        ),
        ModuleInfo::new(
            "issue_old",
            "display list of oldest open issues",
            vec![],
            TriggerType::Keyword(forgejo_config.keywords_issue_old.clone()),
            Some("forgejo communications error"),
            forgejo_config,
            issue_old,
        ),
    ])
}

/// Display list of latest open pull requests.
pub async fn pr_new(_event: ConsumerEvent, _config: ForgejoConfig) -> anyhow::Result<()> {
    Ok(())
}

/// Display list of oldest open pull requests.
pub async fn pr_old(_event: ConsumerEvent, _config: ForgejoConfig) -> anyhow::Result<()> {
    Ok(())
}

/// Display list of latest open issues.
pub async fn issue_new(_event: ConsumerEvent, _config: ForgejoConfig) -> anyhow::Result<()> {
    Ok(())
}

/// Display list of oldest open issues.
pub async fn issue_old(_event: ConsumerEvent, _config: ForgejoConfig) -> anyhow::Result<()> {
    Ok(())
}

pub(crate) fn workers(mx: &Client, config: &Config) -> anyhow::Result<Vec<WorkerInfo>> {
    info!("registering workers");
    let forgejo_config: ForgejoConfig = config.typed_module_config(module_path!())?;
    Ok(vec![WorkerInfo::new(
        "forgejo",
        "observes forgejo organization feeds for configured events",
        "forgejo",
        mx.clone(),
        forgejo_config,
        forgejo_feeds,
    )?])
}

/// Worker spawning forgejo feeds processor in configured intervals.
pub async fn forgejo_feeds(mx: Client, module_config: ForgejoConfig) -> anyhow::Result<()> {
    let mut interval = interval(Duration::from_secs(60 * module_config.feed_interval));
    // (instance name, org)
    let mut first_loop: HashMap<String, bool> = Default::default();
    // (name, instance)
    let mut instances: HashMap<String, Forgejo> = Default::default();
    // (instance name, org)
    let mut activities: HashMap<(String, String), Vec<forgejo_api::structs::Activity>> =
        Default::default();

    for (name, config) in module_config.instances.clone() {
        let auth = Auth::Token(&config.token_secret);
        let forgejo = match Forgejo::new(auth, config.instance_url.parse().unwrap()) {
            Ok(f) => f,
            Err(e) => {
                error!("invalid forgejo configuration: {e}");
                continue;
            }
        };
        instances.insert(name.clone(), forgejo);
        first_loop.insert(name.to_owned(), true);
    }

    loop {
        interval.tick().await;

        for (name, forgejo) in instances.iter() {
            debug!("processing feeds for instance: {name}");

            // can .unwrap(): instances hash is created based on module_config
            let config = module_config.instances.get(name).unwrap();
            let mut potentially_pushed_activities = vec![];

            for org in &config.organizations {
                let query = forgejo_api::structs::OrgListActivityFeedsQuery {
                    date: None,
                    page: None,
                    limit: None,
                };
                let (_, returned_activities) =
                    match forgejo.org_list_activity_feeds(&org, query).await {
                        Ok(v) => v,
                        Err(e) => {
                            error!("error fetching org {org} activity feed: {e}");
                            continue;
                        }
                    };

                let known_act_ids: Vec<i64>;
                let mut new_known_activities = vec![];

                if let Some(known_act) = activities.get(&(name.to_owned(), org.to_owned())) {
                    // can .unwrap(): activites get added to known list only if id.is_some()
                    known_act_ids = known_act.iter().map(|a| a.id.unwrap()).collect();
                } else {
                    activities.insert((name.to_owned(), org.to_owned()), vec![]);
                    known_act_ids = vec![];
                };

                for activity in returned_activities {
                    if let Some(a_id) = activity.id {
                        new_known_activities.push(activity.clone());
                        if !known_act_ids.contains(&a_id) {
                            if activity
                                .op_type
                                .is_some_and(|op| config.events.contains(&op))
                            {
                                potentially_pushed_activities.push(activity);
                            }
                        }
                    }
                }

                activities.insert((name.to_owned(), org.to_owned()), new_known_activities);
            }

            /* it's easier for us to test things if we display first loop
            if first_loop.get(name).unwrap().to_owned() {
                first_loop.insert(name.to_owned(), false);
                continue;
            }
            */

            let mut html_parts: Vec<String> = vec![];
            let mut plain_parts: Vec<String> = vec![];

            for act in potentially_pushed_activities {
                use ActivityOpType::*;

                let maybe_plain_user: String;
                let maybe_html_user: String;

                if act.act_user.clone().is_some_and(|u| u.login.is_some()) {
                    maybe_plain_user = act.act_user.clone().unwrap().login.unwrap();
                    maybe_html_user = if let Some(url) = act.act_user.unwrap().html_url {
                        format!(r#"<a href="{}">{}</a>"#, url.as_str(), maybe_plain_user,)
                    } else {
                        maybe_plain_user.clone()
                    };
                } else {
                    maybe_plain_user = "".s();
                    maybe_html_user = "".s();
                }

                // can .unwrap(): we require this field to exist above
                let action_verb = verb_from_activity_type(act.op_type.unwrap());

                let maybe_plain_target: String;
                let maybe_html_target: String;

                if act
                    .repo
                    .clone()
                    .is_some_and(|r| r.name.is_some() && r.html_url.is_some())
                {
                    maybe_plain_target = format!(
                        "{repo_name}:   ",
                        repo_name = act.repo.clone().unwrap().name.unwrap(),
                    );
                    maybe_html_target = format!(
                        r#"<a href="{url}">{name}</a>"#,
                        url = act.repo.clone().unwrap().html_url.unwrap(),
                        name = maybe_plain_target,
                    );
                } else {
                    maybe_plain_target = "idk where, couldn't decode".s();
                    maybe_html_target = "idk where, couldn't decode".s();
                }

                let maybe_action_content_plain;
                let maybe_action_content_html;

                match act.op_type.clone().unwrap() {
                    CreatePullRequest | ReopenPullRequest if act.content.is_some() => {
                        let content = act.content.unwrap();
                        let mut parts = content.splitn(2, '|');
                        let pr_nr = parts.next().unwrap();
                        let pr_title = if let Some(parts) = parts.next() {
                            unicode_ellipsis::truncate_str(parts, 60).into_owned()
                        } else {
                            "".s()
                        };

                        let pr_url = format!(
                            "{base_url}/pulls/{pr_nr}",
                            base_url = act.repo.unwrap().html_url.unwrap(),
                        );

                        maybe_action_content_plain = format!("#{pr_nr} {pr_title}",);
                        maybe_action_content_html =
                            format!(r#"<a href="{pr_url}">#{pr_nr} {pr_title}</a>"#,);
                    }
                    _ => {
                        maybe_action_content_plain = format!(
                            "sorry, can't handle {:?} properly yet",
                            act.op_type.unwrap()
                        );
                        maybe_action_content_html = format!(
                            "sorry, can't handle {:?} properly yet",
                            act.op_type.unwrap()
                        );
                    }
                };

                let plain_action_message = format!(
                    "{maybe_plain_user} {action_verb} {maybe_plain_target} {maybe_action_content_plain}"
                );
                let html_action_message = format!(
                    "{maybe_html_user} {action_verb} {maybe_html_target} {maybe_action_content_html}"
                );

                plain_parts.push(plain_action_message);
                html_parts.push(html_action_message);
            }

            if plain_parts.is_empty() {
                continue;
            };

            let plain_response = plain_parts.join("\n");
            let html_response = html_parts.join("<br/>");

            for room_name in &config.feed_rooms {
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

/// Processes Forgejo feeds, and sends messages to rooms when applicable.
pub async fn forgejo_feeds_processor(
    _mx: &Client,
    _instance: &ForgejoInstance,
) -> anyhow::Result<()> {
    /* let url = format!(
        "{instance_url}/api/v1/orgs/{organization}/activities/feeds",
    */

    Ok(())
}
