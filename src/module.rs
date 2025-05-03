use crate::prelude::*;
use futures::Future;

pub(crate) async fn init_modules(mx: &Client, config: &Config) -> anyhow::Result<()> {
    let mut modules: Vec<Box<dyn Module>>;

	// let module = modules.first();

    Ok(())
}

pub type Consumer = fn(
    &OriginalSyncRoomMessageEvent,
    &UserId,
    &Room,
) -> dyn Future<Output = anyhow::Result<Option<RoomMessageEventContent>>>;

pub trait Module {
    fn name(&self) -> String;
    fn help(&self) -> String;
    fn decide(&self, sender: &UserId, room: &Room, content: &RoomMessageEventContent) -> bool;
    fn get_consumer(&self) -> Consumer;
}

/*
    async fn consume(
        &self,
        ev: OriginalSyncRoomMessageEvent,
        sender: &UserId,
        room: &Room,
        content: &RoomMessageEventContent,
    ) -> anyhow::Result<Option<RoomMessageEventContent>>;
*/
