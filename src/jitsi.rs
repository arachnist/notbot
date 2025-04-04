use crate::{Config, MODULES};
use linkme::distributed_slice;
use tracing::{debug, info};

use matrix_sdk::{ruma::OwnedRoomId, Client, Room};

use std::collections::HashMap;

// xmpp-specific imports
use tokio_tungstenite::{
    WebSocketStream,
    MaybeTlsStream,
};
use tokio::{
    io::BufStream,
    net::TcpStream,
};

use tokio_xmpp::{
    connect::ServerConnector,
    xmlstream::{
        PendingFeaturesRecv,
        Timeouts,
        initiate_stream,
    },
    jid::Jid,
    Component,
};

use sasl::common::ChannelBinding;

#[distributed_slice(MODULES)]
static JITSI: fn(&Client, &Config) = callback_registrar;

fn callback_registrar(c: &Client, config: &Config) {
    info!("registering jitsi plugin");

    let conference_channel_map: HashMap<String, HashMap<String, String>> = config.module["jitsi"]
        ["Channels"]
        .clone()
        .try_into()
        .expect("conference channel maps need to be defined");
    for (channel, conference) in conference_channel_map.into_iter() {
        debug!("conference: {:#?}", conference);
        let Ok(room_id) = OwnedRoomId::try_from(channel) else {
            todo!()
        };

        let room = match c.get_room(&room_id) {
            Some(r) => r,
            None => break,
        };

        jitsi_observer(
            room,
            conference["server"].clone(),
            conference["room"].clone(),
        );
    }
}

fn jitsi_observer(chat_room: Room, server: String, room: String) {}

// xmpp-specific stuff; mostly yoinked from tokio-xmpp tcp.rs

#[derive(Debug, Clone)]
pub struct WSServerConnector();

/*
impl From<DnsConfig> for WSServerConnector {
    fn from(dns_config: DnsConfig) -> WSServerConnector {
        Self(dns_config)
    }
}
*/

impl ServerConnector for WSServerConnector {
    type Stream = BufStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;

    async fn connect(
        &self,
        jid: &Jid,
        ns: &'static str,
        timeouts: Timeouts,
    ) -> Result<(PendingFeaturesRecv<Self::Stream>, ChannelBinding), tokio_xmpp::Error> {
        Ok(
            (None, ChannelBinding::None)
        )
    }
}
