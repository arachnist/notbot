use crate::prelude::*;

use convert_case::{Case, Casing};
use crc32fast::hash as crc32;
use std::fmt;
use tokio_postgres::types::Type as dbtype;

/*
 * klacz is using a silly clisp ORM: [hu.dwim.perec](https://hub.darcs.net/hu.dwim/hu.dwim.perec)
 * among other things:
 *  * all object classes that are mapped to database/persistence have their own table (normal)
 *  * the tables and all fields are prefixed with an underscore (okay)
 *  *  + 6 views that, suffixed ad, ai, ap, dd, di, dp (no idea what for)
 *  * all tables have an _oid field (weird, but sure) used for storing unique 64bit identifiers
 *  * identifiers in the _oid fields are unique across all persisted classes (!?)
 *
 * to achieve that it uses:
 * 1) a schema-global sequence, `_instance_id`, providing 48 bits of the _oid
 * 2) crc32 derived from class name, providing 16 bits
 *
 * the class name derived stuff is optimistic:
 * ```
 * (def generic compute-class-id (class)
 *  (:method ((class persistent-class))
 *    (bind ((class-name-bytes (string-to-octets (symbol-name (class-name class)) :encoding :utf-8)))
 *      ;; TODO FIXME the probability of clashes is too high this way. e.g. see failing test test/persistence/export/class-id/bug1
 *      (mod (ironclad:octets-to-integer (ironclad:digest-sequence :crc32 class-name-bytes))
 *           +oid-maximum-class-id+))))
 * ```
 *
 * and the code for combining them is:
 * ```
 * (def (function o) make-new-oid (class)
 *  "Creates a fresh and unique oid which was never used before in the relational database."
 *  (or (oid-instance-id-sequence-exists-p *database*)
 *      (ensure-instance-id-sequence))
 *  (bind ((class-id (id-of class))
 *         (instance-id (next-instance-id)))
 *    (class-id-and-instance-id->oid class-id instance-id)))
 *
 * (def (function io) class-id-and-instance-id->oid (class-id instance-id)
 *   (logior (ash instance-id +oid-class-id-bit-size+) class-id))
 * ```
 *
 * `ash` just shifts bits:
 * ```
 * 025025 <ari> ,eval (ash 8361528 16)
 * 025025 <@oof> 547981099008 => (INTEGER 0 4611686018427387903)
 * ```
 *
 * ```
 * irb(main):001> 8361528.to_s(2)
 * => "11111111001011000111000"
 * irb(main):002> 547981099008.to_s(2)
 * => "111111110010110001110000000000000000000"
 * ```
 *
 * I couldn't find the definiton of the function `id-of`, even though
 * it *probably* is somewhere in `hu.dwim.perec`, so i did the next
 * best thing:
 * ```
 * 032210              ari │ ,eval-klacz (hu.dwim.perec::id-of (find-class 'topic-change))
 * 032210             @oof │ 16031 => (INTEGER 0 4611686018427387903)
 * 032435              ari │ ,eval-klacz (hu.dwim.perec::id-of (find-class 'term))
 * 032435             @oof │ 40403 => (INTEGER 0 4611686018427387903)
 * 032438              ari │ ,eval-klacz (hu.dwim.perec::id-of (find-class 'entry))
 * 032438             @oof │ 15414 => (INTEGER 0 4611686018427387903)
 * 032448              ari │ ,eval-klacz (hu.dwim.perec::id-of (find-class 'level))
 * 032448             @oof │ 31841 => (INTEGER 0 4611686018427387903)
 * 032452              ari │ ,eval-klacz (hu.dwim.perec::id-of (find-class 'link))
 * 032452             @oof │ 29822 => (INTEGER 0 4611686018427387903)
 * 032500              ari │ ,eval-klacz (hu.dwim.perec::id-of (find-class 'memo))
 * 032500             @oof │ 7038 => (INTEGER 0 4611686018427387903)
 * 032513              ari │ ,eval-klacz (hu.dwim.perec::id-of (find-class 'seen))
 * 032513             @oof │ 31348 => (INTEGER 0 4611686018427387903)
 * 032846              ari │ ,eval (logior 547981099008 40403)
 * 032846             @oof │ 547981139411 => (INTEGER 0 4611686018427387903)
 * 032858              ari │ ,eval (logior 547981099008 15414)
 * 032858             @oof │ 547981114422 => (INTEGER 0 4611686018427387903)
 * ```
 *
 * ```
 * irb(main):010> Zlib::crc32('TERM') % 65535
 * => 40403
 * irb(main):011> Zlib::crc32('ENTRY') % 65535
 * => 15414
 * ```
 *
 * i still have no idea how id-of gets mapped to compute-class-id. can't find the definition for that.
 * ```
 * 033535              ari │ ,eval-klacz (hu.dwim.perec::compute-class-id (find-class 'term))
 * 033535             @oof │ 40403 => (INTEGER 0 4611686018427387903)
 * 033620              ari │ ,eval-klacz (hu.dwim.perec::compute-class-id (find-class 'entry))
 * 033620             @oof │ 15414 => (INTEGER 0 4611686018427387903)
 * ```
 *
 * ```
 * irb(main):001> 547981099008.to_s(2)
 * => "111111110010110001110000000000000000000"
 * irb(main):002> 40403.to_s(2)
 * => "1001110111010011"
 * irb(main):003> 15414.to_s(2)
 * => "11110000110110"
 * irb(main):004> 547981139411.to_s(2)
 * => "111111110010110001110001001110111010011"
 * irb(main):005> 547981114422.to_s(2)
 * => "111111110010110001110000011110000110110"
 * ```
 */
#[derive(Clone)]
pub struct KlaczDB {
    pub(crate) handle: &'static str,
}

impl KlaczDB {
    const GET_INSTANCE_ID: &str = "select nextval('_instance_id')";
    async fn get_instance_id(&self) -> anyhow::Result<i64> {
        let client = DBPools::get_client(self.handle).await?;
        let statement = client.prepare_cached(Self::GET_INSTANCE_ID).await?;

        let row = client.query_one(&statement, &[]).await?;
        row.try_get(0)
            .map_err(|e: tokio_postgres::Error| anyhow!(e))
    }

    const GET_LEVEL: &str =
        "select _level::bigint from _level where _channel = $1 and _account = $2 limit 1";
    pub async fn get_level(&self, room: &Room, user: &UserId) -> anyhow::Result<i64> {
        let room_name = get_room_name(room);
        let user_name = user.as_str();

        let client = DBPools::get_client(self.handle).await?;
        let statement = client
            .prepare_typed_cached(Self::GET_LEVEL, &[dbtype::VARCHAR, dbtype::VARCHAR])
            .await?;

        // limit in the statement handles (and potentially hides) the case of too many records,
        // while the explicit error handling handles the default case
        let Ok(row) = client
            .query_one(&statement, &[&room_name, &user_name])
            .await
        else {
            return Ok(0);
        };

        row.try_get(0)
            .map_err(|e: tokio_postgres::Error| anyhow!(e))
    }

    // the schema has no unique constraints, so can't do `insert on conflict update`
    const DELETE_LEVELS: &str = "DELETE FROM _level WHERE _channel = $1 and _account = $2";
    const INSERT_LEVELS: &str = r#"INSERT INTO _level (_oid, _channel, _account, _level)
        VALUES ($1, $2, $3, $4)"#;
    pub async fn add_level(&self, room: &Room, user: &UserId, level: i64) -> anyhow::Result<()> {
        let room_name = get_room_name(room);
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

        if transaction
            .execute(&delete, &[&room_name, &user_name])
            .await?
            > 1
        {
            transaction.rollback().await?;
            bail!("too many deleted levels")
        };

        if transaction
            .execute(&insert, &[&oid, &room_name, &user_name, &level])
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

    // this could be done in a single query, but i want better errors
    const GET_TERM_OID: &str = "select _oid from _term where _name = $1";
    const GET_TERM_ENTRY: &str =
        "select _text from _entry where _term_oid = $1 order by random() limit 1";
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

    const REMOVE_TERM: &str = r#"DELETE FROM _term WHERE _oid = $1"#;
    const REMOVE_ENTRY: &str = r#"DELETE FROM _entry
        WHERE _oid = (
            SELECT max(_oid) from _entry where _term_oid = $1 and _text = $2
        )"#;
    const COUNT_ENTRIES: &str = r#"select count(*) from _entry where _term_oid = $1"#;
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

        let deleted = transaction.execute(&remove_entry_statement, &[&term_oid, &entry]).await?;
        if deleted == 0 {
            return Ok(response);
        };

        response = KlaczKBChange::RemovedEntry;

        let left: i64 = transaction.query_one(&count_entries_statement, &[&term_oid]).await?.try_get(0)?;
        if left == 0 {
            let deleted = transaction.execute(&remove_term_statement, &[&term_oid]).await?;
            if deleted > 1 {
                transaction.rollback().await?;
                return Err(KlaczError::DBInconsistency.into());
            };

            response = KlaczKBChange::RemovedTerm;
        };

        transaction.commit().await?;
        Ok(response)
    }

    const INSERT_TERM: &str = r#"INSERT INTO _term (_oid, _name, _visible)
        VALUES ($1, $2, true)"#;
    const INSERT_ENTRY: &str = r#"INSERT INTO _entry (_oid, _term_oid, _added_by, _text, _added_at, _visible)
        VALUES ($1, $2, $3, $4, now(), true)"#;
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

#[allow(dead_code)]
const OID_MAXIMUM_INSTANCE_ID: i64 = 281474976710655;
const OID_MAXIMUM_CLASS_ID: u32 = 65535;

#[derive(Clone, Debug, PartialEq)]
pub enum KlaczKBChange {
    CreatedTerm,
    AddedEntry,
    Unchanged,
    RemovedEntry,
    RemovedTerm,
}

impl fmt::Display for KlaczKBChange {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug)]
pub enum KlaczClass {
    TopicChange,
    Term,
    Entry,
    Level,
    Link,
    Memo,
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
    pub fn class_name(&self) -> String {
        self.to_string().to_case(Case::UpperKebab)
    }

    pub fn class_id(&self) -> i64 {
        (crc32(self.class_name().as_bytes()) % OID_MAXIMUM_CLASS_ID).into()
    }

    pub fn make_oid(&self, instance_id: i64) -> i64 {
        let class_id: i64 = self.class_id();
        let shifted: i64 = instance_id << 16;
        let oid = shifted | class_id;
        trace!("class_id: {class_id}; shifted: {shifted}; instance_id: {instance_id}; oid: {oid}");
        oid
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum KlaczError {
    UnknownClass,
    TermNotFound,
    EntryNotFound,
    DBInconsistency,
}

impl fmt::Display for KlaczError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Clone, Deserialize)]
pub struct ModuleConfig {}

pub(crate) fn modules() -> Vec<ModuleStarter> {
    vec![
        ("notbot::oodkb::add", module_starter_add),
        ("notbot::oodkb::remove", module_starter_remove),
    ]
}

fn module_starter_add(client: &Client, _: &Config) -> anyhow::Result<EventHandlerHandle> {
    Ok(client.add_event_handler(add))
}
fn module_starter_remove(client: &Client, _: &Config) -> anyhow::Result<EventHandlerHandle> {
    Ok(client.add_event_handler(remove))
}

async fn add(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    klacz: Ctx<KlaczDB>,
) -> anyhow::Result<()> {
    if room.state() != RoomState::Joined {
        return Ok(());
    };

    if ev.sender == room.client().user_id().unwrap() {
        return Ok(());
    };

    let MessageType::Text(text) = ev.content.msgtype else {
        return Ok(());
    };

    let mut args = text.body.splitn(3, [' ', '\n']);

    let Some(keyword) = args.next() else {
        return Ok(());
    };

    if keyword != ".add" {
        return Ok(());
    };

    let Some(term) = args.next() else {
        room.send(RoomMessageEventContent::text_plain(
            "missing arguments: term, definition",
        ))
        .await?;
        bail!("missing arguments")
    };

    let Some(definition) = args.next() else {
        room.send(RoomMessageEventContent::text_plain(
            "missing arguments: definition",
        ))
        .await?;
        bail!("missing arguments")
    };

    trace!("attempting to add: term: {term}: definition: {definition}");

    let mut response = String::new();
    let result = klacz.add_entry(&ev.sender, term, definition).await?;
    if result == KlaczKBChange::CreatedTerm {
        response.push_str(format!("Created term \"{term}\"\n").as_str());
    };

    response.push_str(format!(r#"Added one entry to term "{term}""#).as_str());
    room.send(RoomMessageEventContent::text_plain(response))
        .await?;

    Ok(())
}

async fn remove(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    klacz: Ctx<KlaczDB>,
) -> anyhow::Result<()> {
    if room.state() != RoomState::Joined {
        return Ok(());
    };

    if ev.sender == room.client().user_id().unwrap() {
        return Ok(());
    };

    let MessageType::Text(text) = ev.content.msgtype else {
        return Ok(());
    };

    let mut args = text.body.splitn(3, [' ', '\n']);

    let Some(keyword) = args.next() else {
        return Ok(());
    };

    if keyword != ".remove" {
        return Ok(());
    };

    let Some(term) = args.next() else {
        room.send(RoomMessageEventContent::text_plain(
            "missing arguments: term, definition",
        ))
        .await?;
        bail!("missing arguments")
    };

    let Some(definition) = args.next() else {
        room.send(RoomMessageEventContent::text_plain(
            "missing arguments: definition",
        ))
        .await?;
        bail!("missing arguments")
    };

    trace!("attempting to remove: term: {term}: definition: {definition}");

    let response = klacz.remove_entry(term, definition).await?;
    let message = match response {
        KlaczKBChange::Unchanged => format!("entry not found in {term}"),
        KlaczKBChange::RemovedEntry => format!("removed entry from {term}"),
        KlaczKBChange::RemovedTerm => format!("last entry, removed {term}"),
        _ => format!("unexpected response from klacz, no error: {response}"),
    };

    room.send(RoomMessageEventContent::text_plain(message)).await?;

    Ok(())
}
