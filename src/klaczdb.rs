//! KlaczDB module
//!
//! # Configuration
//!
//! [`ModuleConfig`]
//!
//! ```toml
//! [module."notbot::klaczdb"]
//! handle = "main"
//! ```
//!
//! # Usage
//!
//! Keywords:
//! * `add` - [`add_processor`] - adds an entry to the knowledge database
//! * `remove` - [`remove_processor`] - removes a single entry from the knowledge database; removes the term if it removed the last entry
//!
//! # Usage as a module developer
//!
//! Reference to the [`KlaczDB`] object is passed in [`crate::module::ConsumerEvent`], which lets a module developer
//! manipulate the klacz database objects. The functions provided *should* maintain a state consistent with the ORM/persistence
//! layer used by [klacz](https://code.hackerspace.pl/hswaw/klacz)
//!
//! The [`add_processor`] and [`remove_processor`] functions provided in this module use this mechanism.
//!
//! # How this module came to be
//!
//! The [klacz](https://code.hackerspace.pl/hswaw/klacz) irc bot is using a silly clisp ORM: [hu.dwim.perec](https://hub.darcs.net/hu.dwim/hu.dwim.perec)
//! among other things:
//!  * all object classes that are mapped to database/persistence have their own table (normal)
//!  * the tables and all fields are prefixed with an underscore (okay)
//!  *  + 6 views that, suffixed ad, ai, ap, dd, di, dp (no idea what for)
//!  * all tables have an _oid field (weird, but sure) used for storing unique 64bit identifiers
//!  * identifiers in the _oid fields are unique across all persisted classes (!?)
//!
//! to achieve that it uses:
//! 1) a schema-global sequence, `_instance_id`, providing 48 bits of the _oid
//! 2) crc32 derived from class name, providing 16 bits
//!
//! the class name derived stuff is optimistic:
//! ```clisp
//! (def generic compute-class-id (class)
//!  (:method ((class persistent-class))
//!    (bind ((class-name-bytes (string-to-octets (symbol-name (class-name class)) :encoding :utf-8)))
//!      ;; TODO FIXME the probability of clashes is too high this way. e.g. see failing test test/persistence/export/class-id/bug1
//!      (mod (ironclad:octets-to-integer (ironclad:digest-sequence :crc32 class-name-bytes))
//!           +oid-maximum-class-id+))))
//! ```
//!
//! and the code for combining them is:
//! ```clisp
//! (def (function o) make-new-oid (class)
//!  "Creates a fresh and unique oid which was never used before in the relational database."
//!  (or (oid-instance-id-sequence-exists-p *database*)
//!      (ensure-instance-id-sequence))
//!  (bind ((class-id (id-of class))
//!         (instance-id (next-instance-id)))
//!    (class-id-and-instance-id->oid class-id instance-id)))
//!
//! (def (function io) class-id-and-instance-id->oid (class-id instance-id)
//!   (logior (ash instance-id +oid-class-id-bit-size+) class-id))
//! ```
//!
//! `ash` just shifts bits:
//! ```irc
//! 025025 <ari> ,eval (ash 8361528 16)
//! 025025 <@oof> 547981099008 => (INTEGER 0 4611686018427387903)
//! ```
//!
//! ```irb
//! irb(main):001> 8361528.to_s(2)
//! => "11111111001011000111000"
//! irb(main):002> 547981099008.to_s(2)
//! => "111111110010110001110000000000000000000"
//! ```
//!
//! I couldn't find the definiton of the function `id-of`, even though
//! it *probably* is somewhere in `hu.dwim.perec`, so i did the next
//! best thing: queried the the klacz with its exposed eval functionality
//! to see what the unknown function returns.
//!
//! ```irc
//! 032210 <ari> ,eval-klacz (hu.dwim.perec::id-of (find-class 'topic-change))
//! 032210 <oof> 16031 => (INTEGER 0 4611686018427387903)
//! 032435 <ari> ,eval-klacz (hu.dwim.perec::id-of (find-class 'term))
//! 032435 <oof> 40403 => (INTEGER 0 4611686018427387903)
//! 032438 <ari> ,eval-klacz (hu.dwim.perec::id-of (find-class 'entry))
//! 032438 <oof> 15414 => (INTEGER 0 4611686018427387903)
//! 032448 <ari> ,eval-klacz (hu.dwim.perec::id-of (find-class 'level))
//! 032448 <oof> 31841 => (INTEGER 0 4611686018427387903)
//! 032452 <ari> ,eval-klacz (hu.dwim.perec::id-of (find-class 'link))
//! 032452 <oof> 29822 => (INTEGER 0 4611686018427387903)
//! 032500 <ari> ,eval-klacz (hu.dwim.perec::id-of (find-class 'memo))
//! 032500 <oof> 7038 => (INTEGER 0 4611686018427387903)
//! 032513 <ari> ,eval-klacz (hu.dwim.perec::id-of (find-class 'seen))
//! 032513 <oof> 31348 => (INTEGER 0 4611686018427387903)
//! 032846 <ari> ,eval (logior 547981099008 40403)
//! 032846 <oof> 547981139411 => (INTEGER 0 4611686018427387903)
//! 032858 <ari> ,eval (logior 547981099008 15414)
//! 032858 <oof> 547981114422 => (INTEGER 0 4611686018427387903)
//! ```
//!
//! ```irb
//! irb(main):010> Zlib::crc32('TERM') % 65535
//! => 40403
//! irb(main):011> Zlib::crc32('ENTRY') % 65535
//! => 15414
//! ```
//!
//! I then figured out the `id-of` function gets mapped to `compute-class-id`, though I couldn't find the
//! definition for that.
//! ```irb
//! 033535 <ari> ,eval-klacz (hu.dwim.perec::compute-class-id (find-class 'term))
//! 033535 <oof> 40403 => (INTEGER 0 4611686018427387903)
//! 033620 <ari> ,eval-klacz (hu.dwim.perec::compute-class-id (find-class 'entry))
//! 033620 <oof> 15414 => (INTEGER 0 4611686018427387903)
//! ```
//!
//! ```irb
//! irb(main):001> 547981099008.to_s(2)
//! => "111111110010110001110000000000000000000"
//! irb(main):002> 40403.to_s(2)
//! => "1001110111010011"
//! irb(main):003> 15414.to_s(2)
//! => "11110000110110"
//! irb(main):004> 547981139411.to_s(2)
//! => "111111110010110001110001001110111010011"
//! irb(main):005> 547981114422.to_s(2)
//! => "111111110010110001110000011110000110110"
//! ```

use crate::prelude::*;

use convert_case::{Case, Casing};
use crc32fast::hash as crc32;
use std::fmt;
use tokio_postgres::types::Type as dbtype;

#[derive(Clone)]
/// KlaczDB struct
///
/// Just holds a name of the database handle that will be requested from the [`crate::db`] module.
pub struct KlaczDB {
    /// Name of the database handle.
    pub handle: &'static str,
}

/// Functions for interacting with the klacz database in less-naive ways.
impl KlaczDB {
    /// Get next value from the `_instance_id` sequence
    pub const GET_INSTANCE_ID: &str = "SELECT nextval('_instance_id')";

    /// Function for getting next value from the `_instance_id` sequence.
    async fn get_instance_id(&self) -> anyhow::Result<i64> {
        let client = DBPools::get_client(self.handle).await?;
        let statement = client.prepare_cached(Self::GET_INSTANCE_ID).await?;

        let row = client.query_one(&statement, &[]).await?;
        row.try_get(0)
            .map_err(|e: tokio_postgres::Error| anyhow!(e))
    }

    /// Get permission for a given `_channel` and `_account` pair.
    pub const GET_LEVEL: &str = r#"SELECT _level::bigint
        FROM _level
        WHERE
            _channel = $1
            AND _account = $2
        LIMIT 1"#;

    /// Function returning permission level defined in Klacz database for a given user/room pair.
    pub async fn get_level(&self, room: &Room, user: &UserId) -> anyhow::Result<i64> {
        let name = room_name(room);
        let user_name = user.as_str();

        let client = DBPools::get_client(self.handle).await?;
        let statement = client
            .prepare_typed_cached(Self::GET_LEVEL, &[dbtype::VARCHAR, dbtype::VARCHAR])
            .await?;

        // limit in the statement handles (and potentially hides) the case of too many records,
        // while the explicit error handling handles the default case
        let Ok(row) = client.query_one(&statement, &[&name, &user_name]).await else {
            return Ok(0);
        };

        row.try_get(0)
            .map_err(|e: tokio_postgres::Error| anyhow!(e))
    }

    /// Delete existing permission levels for a given `_channel` and `_account` pair.
    pub const DELETE_LEVELS: &str = "DELETE FROM _level WHERE _channel = $1 and _account = $2";
    /// Add new permission level.
    ///
    /// The schema has almost no unique constraints, so `insert on conflict update` can't be used.
    pub const INSERT_LEVELS: &str = r#"INSERT INTO _level (_oid, _channel, _account, _level)
        VALUES ($1, $2, $3, $4)"#;

    /// Function for setting the permission level for a given user/room pair.
    pub async fn add_level(&self, room: &Room, user: &UserId, level: i64) -> anyhow::Result<()> {
        let name = room_name(room);
        let user_name = user.as_str();

        let id = self.get_instance_id().await?;
        let oid = KlaczClass::Level.make_oid(id);

        let mut client = DBPools::get_client(self.handle).await?;
        let transaction = client.transaction().await?;

        let delete = transaction
            .prepare_typed_cached(Self::DELETE_LEVELS, &[dbtype::VARCHAR, dbtype::VARCHAR])
            .await?;
        let insert = transaction
            .prepare_typed_cached(
                Self::INSERT_LEVELS,
                &[dbtype::INT8, dbtype::VARCHAR, dbtype::VARCHAR, dbtype::INT8],
            )
            .await?;

        if transaction.execute(&delete, &[&name, &user_name]).await? > 1 {
            transaction.rollback().await?;
            bail!("too many deleted levels")
        };

        if transaction
            .execute(&insert, &[&oid, &name, &user_name, &level])
            .await?
            != 1
        {
            transaction.rollback().await?;
            bail!("too many inserted levels")
        };

        transaction
            .commit()
            .await
            .map_err(|e: tokio_postgres::Error| anyhow!(e))
    }

    /// Retrieve the `_oid` for term definition, if any.
    pub const GET_TERM_OID: &str = r#"SELECT _oid
        FROM _term
        WHERE _name = $1"#;
    /// Retrieve a random entry for a given term.
    ///
    /// This could be done in a single query, but splitting it into two lets us handle error cases in a more user-friendly way.
    pub const GET_TERM_ENTRY: &str = r#"SELECT _text
        FROM _entry
        WHERE _term_oid = $1
        ORDER BY random()
        LIMIT 1"#;

    /// Retrieve a random entry from Klacz knowledge base
    pub async fn get_entry(&self, term: &str) -> anyhow::Result<String> {
        let client = DBPools::get_client(self.handle).await?;

        let term_statement = client
            .prepare_typed_cached(Self::GET_TERM_OID, &[dbtype::VARCHAR])
            .await?;
        let entry_statement = client
            .prepare_typed_cached(Self::GET_TERM_ENTRY, &[dbtype::INT8])
            .await?;

        let term_rows = client.query(&term_statement, &[&term]).await?;

        let term_oid: i64 = match term_rows.len() {
            0 => return Err(KlaczError::EntryNotFound.into()),
            2.. => return Err(KlaczError::DBInconsistency.into()),
            1 => term_rows.first().unwrap().try_get(0)?,
        };

        let entry_rows = client.query(&entry_statement, &[&term_oid]).await?;
        match entry_rows.len() {
            1 => Ok(entry_rows.first().unwrap().try_get(0)?),
            _ => Err(KlaczError::DBInconsistency.into()),
        }
    }

    /// Remove the term entry, if it no longer has any definitions
    pub const REMOVE_TERM: &str = r#"DELETE FROM _term WHERE _oid = $1"#;
    /// Remove a single instance of a matching entry from the database
    pub const REMOVE_ENTRY: &str = r#"DELETE FROM _entry
        WHERE _oid = (
            SELECT max(_oid)
            FROM _entry
            WHERE
                _term_oid = $1
                AND _text = $2
        )"#;
    /// Count existing entried for a given term
    pub const COUNT_ENTRIES: &str = r#"SELECT count(*) FROM _entry WHERE _term_oid = $1"#;

    /// Remove a signle entry for a given term by its existing content.
    pub async fn remove_entry(&self, term: &str, entry: &str) -> anyhow::Result<KlaczKBChange> {
        let mut response = KlaczKBChange::Unchanged;
        let mut client = DBPools::get_client(self.handle).await?;
        let get_term_statement = client
            .prepare_typed_cached(Self::GET_TERM_OID, &[dbtype::VARCHAR])
            .await?;
        let remove_term_statement = client
            .prepare_typed_cached(Self::REMOVE_TERM, &[dbtype::INT8])
            .await?;
        let remove_entry_statement = client
            .prepare_typed_cached(Self::REMOVE_ENTRY, &[dbtype::INT8, dbtype::VARCHAR])
            .await?;
        let count_entries_statement = client
            .prepare_typed_cached(Self::COUNT_ENTRIES, &[dbtype::INT8])
            .await?;

        let transaction = client.transaction().await?;

        let term_rows = transaction.query(&get_term_statement, &[&term]).await?;
        let term_oid: i64 = match term_rows.len() {
            0 => return Err(KlaczError::TermNotFound.into()),
            2.. => return Err(KlaczError::DBInconsistency.into()),
            1 => term_rows.first().unwrap().try_get(0)?,
        };

        let deleted = transaction
            .execute(&remove_entry_statement, &[&term_oid, &entry])
            .await?;
        if deleted == 0 {
            return Ok(response);
        };

        response = KlaczKBChange::RemovedEntry;

        let left: i64 = transaction
            .query_one(&count_entries_statement, &[&term_oid])
            .await?
            .try_get(0)?;
        if left == 0 {
            let deleted = transaction
                .execute(&remove_term_statement, &[&term_oid])
                .await?;
            if deleted > 1 {
                transaction.rollback().await?;
                return Err(KlaczError::DBInconsistency.into());
            };

            response = KlaczKBChange::RemovedTerm;
        };

        transaction.commit().await?;
        Ok(response)
    }

    /// Add a new term.
    pub const INSERT_TERM: &str = r#"INSERT INTO _term (_oid, _name, _visible)
        VALUES ($1, $2, true)"#;
    /// Add an entry for a given term.
    pub const INSERT_ENTRY: &str = r#"INSERT INTO _entry (_oid, _term_oid, _added_by, _text, _added_at, _visible)
        VALUES ($1, $2, $3, $4, now(), true)"#;

    /// Add entry to the knowledge database
    pub async fn add_entry(
        &self,
        user: &UserId,
        term: &str,
        entry: &str,
    ) -> anyhow::Result<KlaczKBChange> {
        let mut client = DBPools::get_client(self.handle).await?;
        let mut ok_result = KlaczKBChange::AddedEntry;
        let user_name = user.as_str();

        let get_term_statement = client
            .prepare_typed_cached(Self::GET_TERM_OID, &[dbtype::VARCHAR])
            .await?;
        let insert_term_statement = client
            .prepare_typed_cached(Self::INSERT_TERM, &[dbtype::INT8, dbtype::VARCHAR])
            .await?;
        let insert_entry_statement = client
            .prepare_typed_cached(
                Self::INSERT_ENTRY,
                &[dbtype::INT8, dbtype::INT8, dbtype::VARCHAR, dbtype::VARCHAR],
            )
            .await?;

        let transaction = client.transaction().await?;

        let term_rows = transaction.query(&get_term_statement, &[&term]).await?;
        let term_oid: i64 = match term_rows.len() {
            2.. => return Err(KlaczError::DBInconsistency.into()),
            1 => term_rows.first().unwrap().try_get(0)?,
            0 => {
                let term_instance_id = self.get_instance_id().await?;
                let term_oid_new = KlaczClass::Term.make_oid(term_instance_id);

                trace!("term: instance_id: {term_instance_id}, oid: {term_oid_new}");

                transaction
                    .execute(&insert_term_statement, &[&term_oid_new, &term])
                    .await?;
                ok_result = KlaczKBChange::CreatedTerm;

                term_oid_new
            }
        };

        let entry_instance_id = self.get_instance_id().await?;
        let entry_oid = KlaczClass::Entry.make_oid(entry_instance_id);

        trace!("entry: instance_id: {entry_instance_id}, oid: {entry_oid}");
        transaction
            .execute(
                &insert_entry_statement,
                &[&entry_oid, &term_oid, &user_name, &entry],
            )
            .await?;

        transaction.commit().await?;
        Ok(ok_result)
    }
}

/// Maxmium OID value, as defined in the [hu.dwim.perec](https://hub.darcs.net/hu.dwim/hu.dwim.perec) ORM.
#[allow(dead_code)]
pub const OID_MAXIMUM_INSTANCE_ID: i64 = 281474976710655;
/// Maximum value of the class id. Since the class id is stored in the lower
/// 16 bit of the oid, it can't be allowed to grow beyond that.
pub const OID_MAXIMUM_CLASS_ID: u32 = 65535;

#[derive(Clone, Debug, PartialEq)]
/// Possible non-error outcomes of attempting to modify the knowledge database.
pub enum KlaczKBChange {
    /// A new term has been created.
    CreatedTerm,
    /// An entry has been added to an existing term.
    AddedEntry,
    /// No change was made.
    Unchanged,
    /// Entry was removed.
    RemovedEntry,
    /// Term was removed.
    RemovedTerm,
}

impl fmt::Display for KlaczKBChange {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug)]
/// Classes of objects stored in klacz database
///
/// klacz uses the database as a persistence layer for most (all?) of its objects.
/// This is a list of all persisted classes.
pub enum KlaczClass {
    /// Room topic changes
    TopicChange,
    /// Terms of the knowledge database.
    Term,
    /// Entries for terms in knowledge database.
    Entry,
    /// Permission levels.
    Level,
    /// Links that have been posted to rooms.
    Link,
    /// Memos from users to send to other users when they become active.
    Memo,
    /// Information on when was a given user last seen.
    Seen,
}

impl fmt::Display for KlaczClass {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl FromStr for KlaczClass {
    type Err = KlaczError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let binding = s.to_string().to_case(Case::Kebab);
        let unified = binding.as_str();

        trace!("binding: {binding}");
        trace!("unified: {unified}");

        match unified {
            "topic-change" => Ok(Self::TopicChange),
            "term" => Ok(Self::Term),
            "entry" => Ok(Self::Entry),
            "level" => Ok(Self::Level),
            "link" => Ok(Self::Link),
            "memo" => Ok(Self::Memo),
            "seen" => Ok(Self::Seen),
            _ => Err(KlaczError::UnknownClass),
        }
    }
}

impl KlaczClass {
    /// Class name in the form that is used for calculating the class ID
    pub fn class_name(&self) -> String {
        self.to_string().to_case(Case::UpperKebab)
    }

    /// Calculate the class id by taking the appropriately-formated name, calculating a crc32 of it,
    /// and taking the reminder of a division (modulo) of the hash by maximum defined class id.
    /// The modulo operation ensures the result will fit in 16 bits.
    pub fn class_id(&self) -> i64 {
        (crc32(self.class_name().as_bytes()) % OID_MAXIMUM_CLASS_ID).into()
    }

    /// Mix in the provided instance id with calculated class id, to produce an oid value.
    /// Class id for the current class is calculated. The class id always fits in 16 bits.
    /// Provided instance id gets shifted to the left by 16 bits. Class id is stored in freed-up
    /// lower bits using a bitwise-or operation. This step is significant, as the ORM used by
    /// klacz verifies if the oid matches the class it was supposed to come from.
    pub fn make_oid(&self, instance_id: i64) -> i64 {
        let class_id: i64 = self.class_id();
        let shifted: i64 = instance_id << 16;
        let oid = shifted | class_id;
        trace!("class_id: {class_id}; shifted: {shifted}; instance_id: {instance_id}; oid: {oid}");
        oid
    }
}

/// Possible known errors when interacting with Klacz database.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum KlaczError {
    /// Unknown klacz object class.
    UnknownClass,
    /// Term not found in knowledge database.
    TermNotFound,
    /// Entry not found in knowledge database.
    EntryNotFound,
    /// Database state is inconsistent.
    DBInconsistency,
}

impl fmt::Display for KlaczError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

fn default_add_keywords() -> Vec<String> {
    vec!["add".s()]
}

fn default_remove_keywords() -> Vec<String> {
    vec!["remove".s()]
}

/// Module configuration
#[derive(Clone, Deserialize)]
pub struct ModuleConfig {
    /// Keywords to which the add function should respond to
    #[serde(default = "default_add_keywords")]
    pub keywords_add: Vec<String>,
    /// Keywords to which the remove function should respond to
    #[serde(default = "default_remove_keywords")]
    pub keywords_remove: Vec<String>,
}

pub(crate) fn starter(_: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering modules");
    let module_config: ModuleConfig = config.typed_module_config(module_path!())?;

    let (addtx, addrx) = mpsc::channel::<ConsumerEvent>(1);
    let add = ModuleInfo {
        name: "add".s(),
        help: "add an entry to knowledge base".s(),
        acl: vec![],
        trigger: TriggerType::Keyword(module_config.keywords_add.clone()),
        channel: addtx,
        error_prefix: Some("error adding entry".s()),
    };
    add.spawn(addrx, module_config.clone(), add_processor);

    let (removetx, removerx) = mpsc::channel::<ConsumerEvent>(1);
    let remove = ModuleInfo {
        name: "remove".s(),
        help: "remove an entry to knowledge base".s(),
        acl: vec![Acl::KlaczLevel(10)],
        trigger: TriggerType::Keyword(module_config.keywords_remove.clone()),
        channel: removetx,
        error_prefix: Some("error removing entry".s()),
    };
    remove.spawn(removerx, module_config.clone(), remove_processor);

    Ok(vec![add, remove])
}

/// Adds entries/terms to the database.
pub async fn add_processor(event: ConsumerEvent, _: ModuleConfig) -> anyhow::Result<()> {
    let Some(body) = event.args else {
        event
            .room
            .send(RoomMessageEventContent::text_plain(
                "missing arguments: term, definition",
            ))
            .await?;
        bail!("missing arguments")
    };

    let mut args = body.splitn(2, [' ', '\n']);
    let term = args.next().unwrap();
    let Some(definition) = args.next() else {
        event
            .room
            .send(RoomMessageEventContent::text_plain(
                "missing arguments: definition",
            ))
            .await?;
        bail!("missing arguments")
    };

    trace!("attempting to add: term: {term}: definition: {definition}");

    let mut response = String::new();
    let result = event
        .klacz
        .add_entry(&event.sender, term, definition)
        .await?;
    if result == KlaczKBChange::CreatedTerm {
        response.push_str(format!("Created term \"{term}\"\n").as_str());
    };

    response.push_str(format!(r#"Added one entry to term "{term}""#).as_str());
    event
        .room
        .send(RoomMessageEventContent::text_plain(response))
        .await?;

    Ok(())
}

/// Removes entries/terms from the database.
pub async fn remove_processor(event: ConsumerEvent, _: ModuleConfig) -> anyhow::Result<()> {
    let Some(body) = event.args else {
        event
            .room
            .send(RoomMessageEventContent::text_plain(
                "missing arguments: term, definition",
            ))
            .await?;
        bail!("missing arguments")
    };

    let mut args = body.splitn(2, [' ', '\n']);
    let term = args.next().unwrap();
    let Some(definition) = args.next() else {
        event
            .room
            .send(RoomMessageEventContent::text_plain(
                "missing arguments: definition",
            ))
            .await?;
        bail!("missing arguments")
    };

    trace!("attempting to remove: term: {term}: definition: {definition}");

    let response = event.klacz.remove_entry(term, definition).await?;
    let message = match response {
        KlaczKBChange::Unchanged => format!("entry not found in {term}"),
        KlaczKBChange::RemovedEntry => format!("removed entry from {term}"),
        KlaczKBChange::RemovedTerm => format!("last entry, removed {term}"),
        _ => format!("unexpected response from klacz, no error: {response}"),
    };

    event
        .room
        .send(RoomMessageEventContent::text_plain(message))
        .await?;

    Ok(())
}
