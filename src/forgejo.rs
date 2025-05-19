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
//! * `pr-new`, `prnew`, `pr`, `p` - [`forgejo_query`] - show latest open pull requests
//! * `pr-old`, `prold` - [`forgejo_query`] - show oldest open pull requests
//! * `issue-new`, `issuenew`, `issue`, `i` - [`forgejo_query`] - show latest issues
//! * `issue-old`, `issueold` - [`forgejo_query`] - show oldest open issues
//!
//! Like with many other so-called binaries in life, turns out that the PR/issue one is
//! false as well.
//!
//! Passive feed updates: [`forgejo_feeds`]
//! Provides updates about configured events to configured rooms.

use crate::prelude::*;

use std::fmt::Debug;

use tokio::time::{Duration, interval};

use forgejo_api::structs::ActivityOpType;
use forgejo_api::{Auth, Forgejo};

use askama::Template;

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
        ClosePullRequest,
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
    vec!["pr-new".s(), "prnew".s(), "prs".s(), "pr".s()]
}

/// Default keywords for displaying list of oldest open pull requests: pr-old, prold
pub fn keywords_pr_old() -> Vec<String> {
    vec!["pr-old".s(), "prold".s()]
}

/// Default keywords for displaying list of latest open issues: issue-new, issuenew, issue, i
pub fn keywords_issue_new() -> Vec<String> {
    vec![
        "issue-new".s(),
        "issues-new".s(),
        "issuenew".s(),
        "issue".s(),
        "issues".s(),
    ]
}

/// Default keywords for displaying list of oldest open issues: issue-old, issueold
pub fn keywords_issue_old() -> Vec<String> {
    vec!["issue-old".s(), "issues-old".s(), "issueold".s()]
}

pub(crate) fn starter(_: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering modules");
    let forgejo_config: ForgejoConfig = config.typed_module_config(module_path!())?;

    let mut keywords = vec![];
    keywords.extend(forgejo_config.keywords_pr_new.clone());
    keywords.extend(forgejo_config.keywords_pr_old.clone());

    keywords.extend(forgejo_config.keywords_issue_new.clone());
    keywords.extend(forgejo_config.keywords_issue_old.clone());

    Ok(vec![ModuleInfo::new(
        "forgejo-query",
        "display list of open issues or pull requests, oldest or newest",
        vec![],
        TriggerType::Keyword(keywords),
        None,
        forgejo_config.clone(),
        forgejo_query,
    )])
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
            config.instances.values().last().unwrap().to_owned()
        } else {
            event
                .room
                .send(RoomMessageEventContent::text_plain(
                    "No arguments provided and more than one instance configured",
                ))
                .await?;
            return Err(NotProvidedMany.into());
        }
    } else if config
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
                "Argument provided but instance not found",
            ))
            .await?;
        return Err(ProvidedNotFound.into());
    };

    let auth = Auth::Token(&instance.token_secret);
    let forgejo = Forgejo::new(auth, instance.instance_url.parse().unwrap())?;

    Ok((instance.organization, forgejo))
}

/// Display list of open issues or pull requests
pub async fn forgejo_query(event: ConsumerEvent, config: ForgejoConfig) -> anyhow::Result<()> {
    use forgejo_api::structs::*;
    let (organization, forgejo) = early_fail(event.clone(), &config).await?;
    let repo_query = OrgListReposQuery {
        page: Some(1),
        limit: Some(0),
    };

    let query_type = if config.keywords_issue_new.contains(&event.keyword)
        || config.keywords_issue_old.contains(&event.keyword)
    {
        IssueListIssuesQueryType::Issues
    } else {
        IssueListIssuesQueryType::Pulls
    };

    let issue_query = IssueListIssuesQuery {
        state: Some(IssueListIssuesQueryState::Open),
        labels: None,
        q: None,
        r#type: Some(query_type),
        milestones: None,
        since: None,
        before: None,
        created_by: None,
        assigned_by: None,
        mentioned_by: None,
        page: None,
        limit: None,
    };

    let mut items: Vec<Issue> = vec![];

    let (_, repos) = forgejo.org_list_repos(&organization, repo_query).await?;

    for repo in repos {
        if repo.has_issues.is_some_and(|i| i) {
            let (_, repo_issues) = forgejo
                .issue_list_issues(&organization, &repo.name.unwrap(), issue_query.clone())
                .await?;
            items.extend(repo_issues);
        }
    }

    items.sort_by_key(|i| i.updated_at.unwrap());
    let shortlist: Vec<Issue> = if config.keywords_issue_old.contains(&event.keyword)
        || config.keywords_pr_old.contains(&event.keyword)
    {
        items
            .iter()
            .take(config.objects_count.into())
            .map(|x| x.to_owned())
            .collect()
    } else {
        items
            .iter()
            .rev()
            .take(config.objects_count.into())
            .map(|x| x.to_owned())
            .collect()
    };

    let render_items = RenderItems { items: shortlist };
    let message = RoomMessageEventContent::text_html(
        render_items.as_plain().render()?,
        render_items.as_formatted().render()?,
    );

    event.room.send(message).await?;

    Ok(())
}

#[derive(Template)]
#[template(
    path = "matrix/forgejo-formatted.html",
    blocks = ["formatted", "plain"],
)]
struct RenderItems {
    items: Vec<forgejo_api::structs::Issue>,
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

            if potentially_pushed_activities.is_empty() {
                continue;
            }

            let render_feed = activity_fmt::RenderFeed {
                items: potentially_pushed_activities,
            };

            let plain = render_feed.as_plain().render().unwrap();
            let html = render_feed.as_formatted().render().unwrap();

            for room_name in &config.feed_rooms {
                let room = match maybe_get_room(&mx, room_name).await {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                if let Err(e) = room
                    .send(RoomMessageEventContent::text_html(
                        plain.clone(),
                        html.clone(),
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
    use askama::Template;
    use forgejo_api::structs::{Activity, ActivityOpType};
    use reqwest::Url;
    use serde_derive::Deserialize;
    use unicode_ellipsis::truncate_str;

    /// Object for rendering a Matrix message from filtered forgejo feed results.
    #[derive(Template)]
    #[template(
        path = "matrix/forgejo-observer.html",
        blocks = ["formatted", "plain"],
    )]
    pub struct RenderFeed {
        /// List of activities to render
        pub items: Vec<Activity>,
    }

    /// Turns act.content into formatted response part. Applicable for PRs and Issues only
    pub fn act_content_part_html(repo_url: &Url, item: &str, content: String) -> Option<String> {
        let mut parts = content.splitn(3, '|');
        let item_nr = parts.next()?;
        let item_title = if let Some(title) = parts.next() {
            format!(" {}", truncate_str(title, 60))
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

    /// Turns act.content into plain response part. Applicable for PRs and Issues only
    pub fn act_content_part_plain(repo_url: &Url, item: &str, content: String) -> Option<String> {
        let mut parts = content.splitn(3, '|');
        let item_nr = parts.next()?;
        let item_title = if let Some(title) = parts.next() {
            format!(" {}", truncate_str(title, 60))
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

    fn content_commits(s: &str) -> anyhow::Result<ContentCommits> {
        let data: ContentCommits = match serde_json::from_str(s) {
            Ok(d) => d,
            Err(e) => anyhow::bail!("err while parsing commit details from activity: {e}"),
        };

        Ok(data)
    }

    /// Generated from the json that's *sometimes* present in `content` field in [`Activity`]
    #[allow(missing_docs)]
    #[derive(Default, Debug, Clone, Deserialize)]
    #[serde(rename_all = "PascalCase")]
    pub struct ContentCommits {
        pub commits: Vec<Commit>,
        pub head_commit: Commit,
        #[serde(rename = "CompareURL")]
        pub compare_url: String,
        pub len: i64,
    }

    #[allow(missing_docs)]
    #[derive(Default, Debug, Clone, Deserialize)]
    #[serde(rename_all = "PascalCase")]
    pub struct Commit {
        pub sha1: String,
        pub message: String,
        pub author_email: String,
        pub author_name: String,
        pub committer_email: String,
        pub committer_name: String,
        pub timestamp: String,
    }
}
