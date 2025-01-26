use linkme::distributed_slice;

use matrix_sdk::{
    Client,
    Room,
    ruma::events::room::message::SyncRoomMessageEvent,
};

use crate::{CALLBACKS, BotCallback};

#![distributed_slice(crate::CALLBACKS)]
static CALLBACK_AUTOJOINER: BotCallback = BotCallback {
    name: "autojoiner",
    fun: callback,
};

fn callback (c: &Client, r: &Room, s: &SyncRoomMessageEvent) {}
