//! Queries Frogejo for latest, and oldest open issues pull request, and post notifications about new events.
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
    pub organization: String,
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
    pub objects_count: u16,
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
pub fn objects_count() -> u16 {
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

#[derive(thiserror::Error, Debug)]
enum EarlyFailCheck {
    #[error("no instances configured")]
    NoInstancesConfigured,
    #[error("argument provided was not found")]
    ProvidedNotFound,
    #[error("no argument provided and >1 instance configured")]
    NotProvidedMany,
}

async fn early_fail(
    event: ConsumerEvent,
    config: &ForgejoConfig,
) -> anyhow::Result<(String, Forgejo)> {
    use EarlyFailCheck::*;

    let instance = if config.instances.keys().len() == 0 {
        event
            .room
            .send(RoomMessageEventContent::text_plain(
                "No instances configured",
            ))
            .await?;
        return Err(NoInstancesConfigured.into());
    } else if event.args.is_none() {
        if config.instances.keys().len() == 1 {
            config
                .instances
                .iter()
                .map(|(_, v)| v)
                .last()
                .unwrap()
                .to_owned()
        } else {
            event
                .room
                .send(RoomMessageEventContent::text_plain(
                    "No arguments provided and more than one instance configured",
                ))
                .await?;
            return Err(NotProvidedMany.into());
        }
    } else {
        if config
            .instances
            .contains_key(&event.args.clone().ok_or(anyhow!("wtf?"))?)
        {
            config
                .instances
                .get(&event.args.unwrap())
                .unwrap()
                .to_owned()
        } else {
            event
                .room
                .send(RoomMessageEventContent::text_plain(
                    "Argument provided but not found",
                ))
                .await?;
            return Err(ProvidedNotFound.into());
        }
    };

    let auth = Auth::Token(&instance.token_secret);
    let forgejo = Forgejo::new(auth, instance.instance_url.parse().unwrap())?;

    Ok((instance.organization, forgejo))
}

/// Display list of latest open pull requests.
pub async fn pr_new(event: ConsumerEvent, config: ForgejoConfig) -> anyhow::Result<()> {
    use forgejo_api::structs::*;
    let (organization, forgejo) = early_fail(event.clone(), &config).await?;
    let repo_query = OrgListReposQuery {
        page: Some(1),
        limit: Some(0),
    };

    let pr_query = RepoListPullRequestsQuery {
        state: Some(RepoListPullRequestsQueryState::Open),
        sort: None,
        milestone: None,
        labels: None,
        poster: None,
        page: None,
        limit: None,
    };

    let mut pull_requests: Vec<PullRequest> = vec![];

    let (_, repos) = forgejo.org_list_repos(&organization, repo_query).await?;

    for repo in repos {
        if repo.has_pull_requests.is_some_and(|x| x) {
            let (_, pulls) = forgejo
                .repo_list_pull_requests(&organization, &repo.name.unwrap(), pr_query.clone())
                .await?;
            pull_requests.extend(pulls);
        }
    }

    let mut response_parts_html: Vec<String> = vec![];
    let mut response_parts_plain: Vec<String> = vec![];

    pull_requests.sort_by_key(|pr| pr.updated_at.unwrap());

    for pr in pull_requests.iter().rev().take(config.objects_count.into()) {
        trace!("pr: {:#?}", pr.title);
    }
    Ok(())
}

/// Display list of oldest open pull requests.
pub async fn pr_old(event: ConsumerEvent, config: ForgejoConfig) -> anyhow::Result<()> {
    Ok(())
}

/// Display list of latest open issues.
pub async fn issue_new(event: ConsumerEvent, config: ForgejoConfig) -> anyhow::Result<()> {
    Ok(())
}

/// Display list of oldest open issues.
pub async fn issue_old(event: ConsumerEvent, config: ForgejoConfig) -> anyhow::Result<()> {
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
            trace!("processing feeds for instance: {name}");

            // can .unwrap(): instances hash is created based on module_config
            let config = module_config.instances.get(name).unwrap();
            let mut potentially_pushed_activities = vec![];

            let org = &config.organization;

            let query = forgejo_api::structs::OrgListActivityFeedsQuery {
                date: None,
                page: None,
                limit: None,
            };
            let (_, returned_activities) = match forgejo.org_list_activity_feeds(org, query).await {
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
                    if !known_act_ids.contains(&a_id)
                        && activity
                            .op_type
                            .is_some_and(|op| config.events.contains(&op))
                    {
                        potentially_pushed_activities.push(activity);
                    }
                }
            }

            activities.insert((name.to_owned(), org.to_owned()), new_known_activities);

            if first_loop.get(name).unwrap().to_owned() {
                first_loop.insert(name.to_owned(), false);

                continue;
            }

            let mut html_parts: Vec<String> = vec![];
            let mut plain_parts: Vec<String> = vec![];

            for act in potentially_pushed_activities {
                let op_type = act.clone().op_type;

                let plain_action_message = match activity_fmt::message_plain(act.clone()) {
                    Some(message) => message,
                    None => {
                        format!("uh oh, {:?} for plain is broken", op_type)
                    }
                };

                let html_action_message = match activity_fmt::message_html(act) {
                    Some(message) => message,
                    None => {
                        format!(
                            "uh oh, apparently i don't handle {:?} for html; plain: {}",
                            op_type,
                            plain_action_message.clone()
                        )
                    }
                };

                plain_parts.push(plain_action_message);
                html_parts.push(html_action_message);
            }

            if plain_parts.is_empty() {
                continue;
            };

            let plain_response = plain_parts.join("\n");
            let html_response = html_parts.join("<br/>");

            for room_name in &config.feed_rooms {
                let room = match maybe_get_room(&mx, room_name).await {
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

pub mod activity_fmt {
    //! Formating forgejo feed activities, separated into its own module.
    use crate::tools::ToStringExt;
    use forgejo_api::structs::{Activity, ActivityOpType, User};
    use reqwest::Url;
    use unicode_ellipsis::truncate_str;

    /// Turns the ActivityOpType into a human readable string, admittedly somewhat naively.
    pub fn activity_descr(act: ActivityOpType) -> String {
        use ActivityOpType::*;
        match act {
            // act on repo
            CreateRepo => "created repository",
            RenameRepo => "renamed repository",
            StarRepo => "starred",
            WatchRepo => "started watching",
            CommitRepo => "commited to",
            TransferRepo => "transferred",
            PushTag => "pushed tag in",
            DeleteTag => "deleted tag in",
            DeleteBranch => "deleted branch in",
            PublishRelease => "has published release for",

            // act on issue
            CreateIssue => "created issue in",
            CommentIssue => "commented on issue",
            CloseIssue => "closed issue in",
            ReopenIssue => "reopened issue in",

            // act on pull request
            CreatePullRequest => "created pull request in",
            MergePullRequest => "merged pull request in",
            ClosePullRequest => "closed pull request in",
            ReopenPullRequest => "reopened pull request in",
            ApprovePullRequest => "has approved pull request in",
            RejectPullRequest => "has rejected pull request in",
            CommentPull => "has commented on pull request in",
            PullReviewDismissed => "has dismissed pull review in",
            PullRequestReadyForReview => "has marked pull request as ready for review in",
            AutoMergePullRequest => "has automatically merged pull request in",

            MirrorSyncPush => "mirror operation has pushed sync to",
            MirrorSyncCreate => "mirror has created",
            MirrorSyncDelete => "mirror has deleted",
            // _ => todo!(), rustc doesn't like the possibility that forgejo might implement more features ;)
        }
        .s()
    }

    /// Renders a single Activity to matrix-acceptable HTML
    pub fn message_html(act: Activity) -> Option<String> {
        use ActivityOpType::*;
        let mut response_parts: Vec<String> = vec![];

        let act_user = act.act_user?;
        if act_user.id? > 0 {
            response_parts.push(format!(
                r#"<a href="{}">{}</a>"#,
                act_user.clone().html_url?,
                act_user_display_name(act_user)?,
            ));
        } else {
            response_parts.push(act_user.login?);
        };

        response_parts.push("has".s());
        let act_op = act.op_type?;
        response_parts.push(activity_descr(act_op));

        let act_repo = act.repo?;
        let act_repo_url = act_repo.html_url?;
        response_parts.push(format!(
            r#"<a href="{act_repo_url}">{name}</a>   "#,
            name = act_repo.name?,
        ));

        match act_op {
            CreatePullRequest | MergePullRequest | ClosePullRequest | ReopenPullRequest
            | ApprovePullRequest | RejectPullRequest | CommentPull | PullReviewDismissed
            | AutoMergePullRequest => {
                let content = act.content?;
                response_parts.push(act_content_part_html(act_repo_url, "pulls", content)?)
            }
            CreateIssue | CommentIssue | CloseIssue | ReopenIssue => {
                let content = act.content?;
                response_parts.push(act_content_part_html(act_repo_url, "issues", content)?)
            }
            _ => (),
        };

        Some(response_parts.join(" "))
    }

    /// Turns act.content into formatted response part. Applicable for PRs and Issues only
    pub fn act_content_part_html(repo_url: Url, item: &str, content: String) -> Option<String> {
        let mut parts = content.splitn(3, '|');
        let item_nr = parts.next()?;
        let item_title = if let Some(title) = parts.next() {
            format!(" {}", truncate_str(title, 40))
        } else {
            "".s()
        };
        let item_emoji = if let Some(emoji) = parts.next() {
            format!(" {}", emoji)
        } else {
            "".s()
        };

        let item_url = format!("{repo_url}/{item}/{item_nr}");
        Some(format!(
            r#"<a href="{item_url}">#{item_nr}{item_title}{item_emoji}</a>"#,
        ))
    }

    /// Renders a single Activity to hopefully appservice-irc-acceptable plain text
    pub fn message_plain(act: Activity) -> Option<String> {
        use ActivityOpType::*;
        let mut response_parts: Vec<String> = vec![];

        let act_user = act.act_user?;
        response_parts.push(act_user_display_name(act_user)?);

        response_parts.push("has".s());
        let act_op = act.op_type?;
        response_parts.push(activity_descr(act_op));

        let act_repo = act.repo?;
        let act_repo_url = act_repo.html_url?;
        response_parts.push(format!("{}:", act_repo.name?));

        match act_op {
            CreatePullRequest | MergePullRequest | ClosePullRequest | ReopenPullRequest
            | ApprovePullRequest | RejectPullRequest | CommentPull | PullReviewDismissed
            | AutoMergePullRequest => {
                let content = act.content?;
                response_parts.push(act_content_part_plain(act_repo_url, "pulls", content)?)
            }
            CreateIssue | CommentIssue | CloseIssue | ReopenIssue => {
                let content = act.content?;
                response_parts.push(act_content_part_plain(act_repo_url, "issues", content)?)
            }
            _ => (),
        };

        Some(response_parts.join(" "))
    }

    /// Turns act.content into plain response part. Applicable for PRs and Issues only
    pub fn act_content_part_plain(repo_url: Url, item: &str, content: String) -> Option<String> {
        let mut parts = content.splitn(3, '|');
        let item_nr = parts.next()?;
        let item_title = if let Some(title) = parts.next() {
            format!(" {}", truncate_str(title, 40))
        } else {
            "".s()
        };
        let item_emoji = if let Some(emoji) = parts.next() {
            format!(" {}", emoji)
        } else {
            "".s()
        };

        let item_url = format!("{repo_url}/{item}/{item_nr}");
        Some(format!(r#"{item_url}{item_title}{item_emoji}"#,))
    }

    /// Shortens username if necessary, for display purposes.
    pub fn act_user_short(user: User) -> Option<String> {
        Some(truncate_str(user.login?.as_str(), 20).into_owned())
    }

    /// Shows user configured name or shortened username.
    pub fn act_user_display_name(user: User) -> Option<String> {
        if user.clone().full_name.is_some_and(|n| !n.is_empty()) {
            Some(user.full_name?.trim().to_owned())
        } else {
            act_user_short(user)
        }
    }
}
