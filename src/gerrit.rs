use crate::prelude::*;

use gerrit_api::{gerrit_decode_json, Changes};
use reqwest::ClientBuilder;
use tokio::time::{interval, Duration};

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

            let data = match async || -> anyhow::Result<Changes> {
                let text = client.get(changes_url).send().await?.text().await?;
                let data: Changes = gerrit_decode_json(text)?;
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

#[allow(missing_docs)]
pub mod gerrit_api {
    use anyhow::bail;
    use serde::de;
    use serde_derive::Deserialize;
    use serde_json::Value;

    pub fn gerrit_decode_json<D: de::DeserializeOwned>(text: String) -> anyhow::Result<D> {
        match text.strip_prefix(")]}'") {
            None => bail!("this does not look like a gerrit response"),
            Some(data) => Ok(serde_json::from_str(data)?),
        }
    }

    // https://gerrit.hackerspace.pl/accounts/…/name
    pub type Name = String;

    // https://gerrit.…/changes/?q=…
    pub type Changes = Vec<Change>;

    #[derive(Default, Debug, Clone, PartialEq, Deserialize)]
    pub struct Change {
        pub id: String,
        pub project: String,
        pub branch: String,
        pub hashtags: Vec<Value>,
        pub change_id: String,
        pub subject: String,
        pub status: String,
        pub created: String,
        pub updated: String,
        pub submit_type: String,
        pub insertions: i64,
        pub deletions: i64,
        pub total_comment_count: i64,
        pub unresolved_comment_count: i64,
        pub has_review_started: bool,
        pub meta_rev_id: String,
        #[serde(rename = "_number")]
        pub number: u64,
        pub owner: Owner,
        pub requirements: Vec<Value>,
    }

    #[derive(Default, Debug, Clone, PartialEq, Deserialize)]
    pub struct Owner {
        #[serde(rename = "_account_id")]
        pub account_id: u64,
    }
}
