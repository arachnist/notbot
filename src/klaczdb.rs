use crate::prelude::*;

use deadpool_postgres::Client as DBClient;

struct klaczdb {
    dbclient: DBClient,
}

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
 *
 */
impl klaczdb {
}

#[derive(Clone, Deserialize)]
pub struct ModuleConfig {
    pub handle: String,
}

pub(crate) fn modules() -> Vec<ModuleStarter> {
    vec![("notbot::oodkb::add", module_starter_add)]
}

fn module_starter_add(client: &Client, config: &Config) -> anyhow::Result<EventHandlerHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
    Ok(client.add_event_handler(move |ev, room| add(ev, room, module_config)))
}

async fn add(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    config: ModuleConfig,
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
        room.send(RoomMessageEventContent::text_plain("missing arguments: term, definition")).await?;
        return Err(anyhow::Error::msg("missing arguments"));
    };
    
    let Some(definition) = args.next() else {
        room.send(RoomMessageEventContent::text_plain("missing arguments: definition")).await?;
        return Err(anyhow::Error::msg("missing arguments"));
    };

    room.send(RoomMessageEventContent::text_plain(format!("would add: term: {term}: definition: {definition}"))).await?;
    Ok(())
}
