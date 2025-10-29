//! `KlaczDB` module
//!
//! # Configuration
//!
//! [`ModuleConfig`]
//!
//! ```toml
//! [module."notbot::points"]
//! handle = "notbot"
//! ```
//!
//! # Usage
//!
//! This module is, primarily, a catch-all module - adjusting scores in the database when someone writes `word++` or `word--` on a channel where the bot is
//! present, but also exposes a few keywords:
//! * `inc` - for increasing the score
//! * `dec` - for decreasing the score
//! * `score` - check score of a given term
//! * `reset` - admin-level command to reset score to 0

use crate::prelude::*;

use tokio_postgres::types::Type as dbtype;

#[derive(Clone)]
/// `PointsDB` struct
///
/// Just holds a name of the database handle that will be requested from the [`crate::db`] module.
pub struct PointsDB {
    /// Name of the database handle.
    pub handle: String,
}

fn default_increment_keywords() -> Vec<String> {
    vec!["inc".s()]
}

fn default_decrement_keywords() -> Vec<String> {
    vec!["dec".s()]
}

fn default_score_check_keywords() -> Vec<String> {
    vec!["score".s()]
}

fn default_reset_keywords() -> Vec<String> {
    vec!["score-reset".s()]
}

fn default_handle() -> String {
    "notbot".s()
}

/// Module configuration
#[derive(Clone, Deserialize)]
pub struct ModuleConfig {
    /// Keywords to which the increment function should respond to
    #[serde(default = "default_increment_keywords")]
    pub keywords_inc: Vec<String>,
    /// Keywords to which the decrement function should respond to
    #[serde(default = "default_decrement_keywords")]
    pub keywords_dec: Vec<String>,
    /// Keywords to which the score check function should respond to
    #[serde(default = "default_score_check_keywords")]
    pub keywords_check: Vec<String>,
    /// Keywords to which the reset function should respond to
    #[serde(default = "default_reset_keywords")]
    pub keywords_reset: Vec<String>,
    /// Klacz DB handle
    #[serde(default = "default_handle")]
    pub handle: String,
}

/// Functions for interacting with the points database in less naive ways
impl PointsDB {
    /// Adjust term score query
    pub const ADJ_SCORE: &str = r"INSERT INTO points_scores (term, score)
    VALUES ($1, $2)
    ON CONFLICT (term) DO UPDATE
    SET score = points_scores.score + $2";

    /// Function for adjusting the score of a given term.
    ///
    /// # Errors
    /// Will return `Err` if underlying database operations fail.
    pub async fn adj_score(&self, term: &str, amount: i64) -> anyhow::Result<()> {
        let mut client = DBPools::get_client(&self.handle).await?;
        let transaction = client.transaction().await?;
        let adjust_score = transaction
            .prepare_typed_cached(Self::ADJ_SCORE, &[dbtype::VARCHAR, dbtype::INT8])
            .await?;

        if transaction
            .execute(&adjust_score, &[&term, &amount])
            .await?
            != 1
        {
            transaction.rollback().await?;
            bail!("too many inserter rows")
        }

        transaction
            .commit()
            .await
            .map_err(|e: tokio_postgres::Error| anyhow!(e))
    }

    /// Increment term score query
    pub const RESET_SCORE: &str = r"INSERT INTO points_scores (term, score)
    VALUES ($1, 0)
    ON CONFLICT (term) DO UPDATE
    SET score = 0";

    /// Function for resetting the score of a given term.
    ///
    /// # Errors
    /// Will return `Err` if underlying database operations fail.
    pub async fn reset_score(&self, term: &str) -> anyhow::Result<()> {
        let mut client = DBPools::get_client(&self.handle).await?;
        let transaction = client.transaction().await?;
        let reset_score = transaction
            .prepare_typed_cached(Self::RESET_SCORE, &[dbtype::VARCHAR])
            .await?;

        if transaction.execute(&reset_score, &[&term]).await? != 1 {
            transaction.rollback().await?;
            bail!("too many inserter rows")
        }

        transaction
            .commit()
            .await
            .map_err(|e: tokio_postgres::Error| anyhow!(e))
    }

    /// Get term score query
    pub const GET_SCORE: &str = r"SELECT score::bigint FROM points_scores WHERE term = $1";

    /// Function for retrieving current term score
    ///
    /// # Errors
    /// Will return `Err` if underlying database operations fail.
    pub async fn get_score(&self, term: &str) -> anyhow::Result<i64> {
        let client = DBPools::get_client(&self.handle).await?;

        let statement = client
            .prepare_typed_cached(Self::GET_SCORE, &[dbtype::VARCHAR])
            .await?;

        let score_rows = client.query(&statement, &[&term]).await?;

        trace!("score rows: {:#?}", score_rows);
        match score_rows.len() {
            0 => Ok(0),
            1 => Ok(score_rows
                .first()
                .ok_or_else(|| anyhow!("no row returned despite len() == 1"))?
                .try_get(0)?),
            _ => bail!("too many returned rows"),
        }
    }

    /// Query for retrieving next time a user can use the points system.
    pub const GET_USE_TIME: &str = r"SELECT next_use FROM points_ratelimit WHERE mxid = $1";

    /// `UPSERT` query for setting next time a user can use the points system.
    pub const SET_USE_TIME: &str = r"INSERT INTO points_ratelimit (mxid, next_use)
    VALUES ( $1, $2 )
    ON CONFLICT (mxid) DO UPDATE
        SET next_use = $2";

    /// Function to return next time a user can use the points system.
    pub async fn next_use_time(&self, mxid: &str) -> anyhow::Result<SystemTime> {
        let now = SystemTime::now();
        let future_time = now.checked_add(Duration::from_secs(60)).map_or(now, |e| e);

        let mut client = DBPools::get_client(&self.handle).await?;
        let transaction = client.transaction().await?;

        let get_use = transaction
            .prepare_typed_cached(Self::GET_USE_TIME, &[dbtype::VARCHAR])
            .await?;
        let set_use = transaction
            .prepare_typed_cached(Self::SET_USE_TIME, &[dbtype::VARCHAR, dbtype::TIMESTAMP])
            .await?;

        let use_q = transaction.query(&get_use, &[&mxid]).await?;
        let nuse = match use_q.len() {
            0 => {
                transaction
                    .execute(&set_use, &[&mxid, &future_time])
                    .await?;

                SystemTime::UNIX_EPOCH
            }
            1 => {
                let t = match use_q.first() {
                    None => bail!("wtf? db inconsistency: can't fetch first result row"),
                    Some(r) => r.try_get(0)?,
                };

                if t < now {
                    transaction
                        .execute(&set_use, &[&mxid, &future_time])
                        .await?;
                };

                t
            }
            _ => bail!("wtf? db inconsistency: multiple entries per member in points_ratelimit"),
        };

        transaction.commit().await?;
        Ok(nuse)
    }
}

/// Checks ratelimit and adjusts score.
///
/// # Errors
/// Will return `Err` if database manipulation fails.
pub async fn adj_score(mxid: &str, term: &str, amount: i64, c: ModuleConfig) -> anyhow::Result<()> {
    let points = PointsDB { handle: c.handle };

    let next_use_time = points.next_use_time(mxid).await?;

    if SystemTime::now() < next_use_time {
        return Ok(());
    };

    points.adj_score(term, amount).await?;

    Ok(())
}

/// Increments the score of a given term
///
/// # Errors
/// Will return `Err` if database manipulation fails.
pub async fn inc_processor(event: ConsumerEvent, c: ModuleConfig) -> anyhow::Result<()> {
    let Some(body) = event.args else {
        event
            .room
            .send(RoomMessageEventContent::text_plain(
                "missing arguments: term",
            ))
            .await?;
        bail!("missing arguments")
    };

    let mut args = body.split_ascii_whitespace();
    let term = args.next().ok_or_else(|| anyhow!("term missing"))?;

    adj_score(event.sender.as_str(), term, 1, c).await
}

/// Decrements the score of a given term
///
/// # Errors
/// Will return `Err` if database manipulation fails.
pub async fn dec_processor(event: ConsumerEvent, c: ModuleConfig) -> anyhow::Result<()> {
    let Some(body) = event.args else {
        event
            .room
            .send(RoomMessageEventContent::text_plain(
                "missing arguments: term",
            ))
            .await?;
        bail!("missing arguments")
    };

    let mut args = body.split_ascii_whitespace();
    let term = args.next().ok_or_else(|| anyhow!("term missing"))?;

    adj_score(event.sender.as_str(), term, -1, c).await
}

/// Checks the score of a given term
///
/// # Errors
/// Will return `Err` if database access fails.
pub async fn score_processor(event: ConsumerEvent, c: ModuleConfig) -> anyhow::Result<()> {
    let Some(body) = event.args else {
        event
            .room
            .send(RoomMessageEventContent::text_plain(
                "missing arguments: term",
            ))
            .await?;
        bail!("missing arguments")
    };

    let mut args = body.split_ascii_whitespace();
    let term = args.next().ok_or_else(|| anyhow!("missing arguments"))?;

    let points = PointsDB { handle: c.handle };

    let response = format!(
        "current score for \"{term}\" is {}",
        points.get_score(term).await?
    );

    event
        .room
        .send(RoomMessageEventContent::text_plain(response))
        .await?;

    Ok(())
}

/// Resets the score of a given term
///
/// # Errors
/// Will return `Err` if database manipulation fails.
pub async fn reset_processor(event: ConsumerEvent, c: ModuleConfig) -> anyhow::Result<()> {
    let Some(body) = event.args else {
        event
            .room
            .send(RoomMessageEventContent::text_plain(
                "missing arguments: term",
            ))
            .await?;
        bail!("missing arguments")
    };

    let mut args = body.split_ascii_whitespace();
    let term = args.next().ok_or_else(|| anyhow!("missing arguments"))?;

    let points = PointsDB { handle: c.handle };

    points.reset_score(term).await?;

    let response = format!("score for {term} has been reset to 0");

    event
        .room
        .send(RoomMessageEventContent::text_plain(response))
        .await?;

    Ok(())
}

pub(crate) fn starter(_: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    let module_config: ModuleConfig = config.typed_module_config(module_path!())?;

    Ok(vec![
        ModuleInfo::new(
            "inc",
            "increment score of a given term",
            vec![],
            TriggerType::Keyword(module_config.keywords_inc.clone()),
            Some("error manipulating score"),
            module_config.clone(),
            inc_processor,
        ),
        ModuleInfo::new(
            "dec",
            "decrement score of a given term",
            vec![],
            TriggerType::Keyword(module_config.keywords_dec.clone()),
            Some("error manipulating score"),
            module_config.clone(),
            dec_processor,
        ),
        ModuleInfo::new(
            "score",
            "check score of a given term",
            vec![],
            TriggerType::Keyword(module_config.keywords_check.clone()),
            Some("error retrieving score"),
            module_config.clone(),
            score_processor,
        ),
        ModuleInfo::new(
            "reset",
            "reset score of a given term",
            vec![Acl::SpecificUsers(config.admins())],
            TriggerType::Keyword(module_config.keywords_reset.clone()),
            Some("error resetting score"),
            module_config.clone(),
            reset_processor,
        ),
    ])
}

fn inc_decider(
    _: i64,
    _: OwnedUserId,
    _: &Room,
    content: &RoomMessageEventContent,
    _: &Config,
) -> anyhow::Result<Consumption> {
    match &content.msgtype {
        MessageType::Text(content_text) => {
            let words: Vec<String> = content_text
                .body
                .to_string()
                .split_ascii_whitespace()
                .map(str::to_string)
                .collect();

            if words.len() == 1 {
                if let Some(word) = words.first() {
                    if word.ends_with("++") {
                        return Ok(Consumption::Passthrough);
                    }
                }
            }

            Ok(Consumption::Reject)
        }
        _ => Ok(Consumption::Reject),
    }
}

/// Increments the score of a given term
///
/// # Errors
/// Will return `Err` if database manipulation fails.
pub async fn inc_passthrough_processor(
    event: ConsumerEvent,
    c: ModuleConfig,
) -> anyhow::Result<()> {
    let MessageType::Text(content_text) = event.ev.content.msgtype else {
        return Ok(());
    };

    let words: Vec<String> = content_text
        .body
        .to_string()
        .split_ascii_whitespace()
        .map(str::to_string)
        .collect();

    if words.len() == 1 {
        if let Some(word) = words.first() {
            if word.ends_with("++") {
                if let Some(term) = word.strip_suffix("++") {
                    return adj_score(event.sender.as_str(), term, 1, c).await;
                }
            }
        }
    }

    Ok(())
}

fn dec_decider(
    _: i64,
    _: OwnedUserId,
    _: &Room,
    content: &RoomMessageEventContent,
    _: &Config,
) -> anyhow::Result<Consumption> {
    match &content.msgtype {
        MessageType::Text(content_text) => {
            let words: Vec<String> = content_text
                .body
                .to_string()
                .split_ascii_whitespace()
                .map(str::to_string)
                .collect();

            if words.len() == 1 {
                if let Some(word) = words.first() {
                    if word.ends_with("--") {
                        return Ok(Consumption::Passthrough);
                    }
                }
            }

            Ok(Consumption::Reject)
        }
        _ => Ok(Consumption::Reject),
    }
}

/// Decrements the score of a given term
///
/// # Errors
/// Will return `Err` if database manipulation fails.
pub async fn dec_passthrough_processor(
    event: ConsumerEvent,
    c: ModuleConfig,
) -> anyhow::Result<()> {
    let MessageType::Text(content_text) = event.ev.content.msgtype else {
        return Ok(());
    };

    let words: Vec<String> = content_text
        .body
        .to_string()
        .split_ascii_whitespace()
        .map(str::to_string)
        .collect();

    if words.len() == 1 {
        if let Some(word) = words.first() {
            if word.ends_with("--") {
                if let Some(term) = word.strip_suffix("--") {
                    return adj_score(event.sender.as_str(), term, -1, c).await;
                }
            }
        }
    }

    Ok(())
}

pub(crate) fn passthrough(
    _: &Client,
    config: &Config,
) -> anyhow::Result<Vec<PassThroughModuleInfo>> {
    info!("registering passthrough modules");
    let module_config: ModuleConfig = config.typed_module_config(module_path!())?;

    Ok(vec![
        PassThroughModuleInfo(ModuleInfo::new(
            "inc++",
            "increment score through term++ messages",
            vec![],
            TriggerType::Catchall(inc_decider),
            None,
            module_config.clone(),
            inc_passthrough_processor,
        )),
        PassThroughModuleInfo(ModuleInfo::new(
            "inc++",
            "increment score through term++ messages",
            vec![],
            TriggerType::Catchall(dec_decider),
            None,
            module_config.clone(),
            dec_passthrough_processor,
        )),
    ])
}
