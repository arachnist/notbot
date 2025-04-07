use crate::{fetch_and_decode_json, Config, MODULES};

use tracing::{error, info, trace};

use linkme::distributed_slice;
use matrix_sdk::{
    ruma::events::room::message::{
        MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
    },
    Client, Room,
};

use serde_derive::Deserialize;
use serde_json::Value;
use urlencoding::encode as uencode;

#[distributed_slice(MODULES)]
static WOLFRAM: fn(&Client, &Config) = callback_registrar;

fn callback_registrar(c: &Client, config: &Config) {
    info!("registering wolfram");

    let app_id = match config.module["wolfram"]["AppID"].clone().try_into() {
        Ok(a) => a,
        Err(_) => {
            error!("Couldn't load App ID from configuration");
            return;
        }
    };

    c.add_event_handler(move |ev, room| wolfram_response(ev, room, app_id));
}

async fn wolfram_response(ev: OriginalSyncRoomMessageEvent, room: Room, app_id: String) {
    trace!("in wolfram");

    trace!("checking message type");
    let MessageType::Text(text) = ev.content.msgtype else {
        return;
    };

    trace!("checking if message starts with .c: {:#?}", text.body);
    if text.body.trim().starts_with(".c ") {
        tokio::spawn(async move {
            let text_query = text.body.trim().strip_prefix(".c ").unwrap();
            let query = uencode(text_query);

            let url: String = "http://api.wolframalpha.com/v2/query?input=".to_owned()
                + query.as_ref()
                + "&appid="
                + app_id.as_str()
                + "&output=json";

            let data = match fetch_and_decode_json::<WolframAlpha>(url).await {
                Ok(d) => d,
                Err(fe) => {
                    error!("error fetching data: {fe}");
                    if let Err(se) = room
                        .send(RoomMessageEventContent::text_plain("couldn't fetch data"))
                        .await
                    {
                        error!("error sending response: {se}");
                    };
                    return;
                }
            };

            if !data.queryresult.success || data.queryresult.numpods == 0 {
                if let Err(e) = room
                    .send(RoomMessageEventContent::text_plain("no results"))
                    .await
                {
                    error!("error sending response: {e}");
                };
                return;
            };

            trace!("wolfram data: {:#?}", data);

            let mut response_parts: Vec<String> = vec![];

            for pod in data.queryresult.pods {
                if pod.primary.is_some_and(|x| x) {
                    response_parts.push(pod.title + ": " + pod.subpods[0].plaintext.as_str());
                }
            }

            let response = RoomMessageEventContent::text_plain(response_parts.join("\n"));
            if let Err(e) = room.send(response).await {
                error!("error sending response: {e}");
                return;
            }
        });
    };
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct WolframAlpha {
    pub queryresult: Queryresult,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct Queryresult {
    pub success: bool,
    pub error: bool,
    pub numpods: i64,
    pub datatypes: String,
    pub timedout: String,
    pub timedoutpods: String,
    pub timing: f64,
    pub parsetiming: f64,
    pub parsetimedout: bool,
    pub recalculate: String,
    pub id: String,
    pub host: String,
    pub server: String,
    pub related: String,
    pub version: String,
    pub inputstring: String,
    pub pods: Vec<Pod>,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct Pod {
    pub title: String,
    pub scanner: String,
    pub id: String,
    pub position: i64,
    pub error: bool,
    pub numsubpods: i64,
    pub subpods: Vec<Subpod>,
    pub expressiontypes: Value,
    pub primary: Option<bool>,
    #[serde(default)]
    pub states: Vec<State>,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct Subpod {
    pub title: String,
    pub img: Img,
    pub plaintext: String,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct Img {
    pub src: String,
    pub alt: String,
    pub title: String,
    pub width: i64,
    pub height: i64,
    #[serde(rename = "type")]
    pub type_field: String,
    pub themes: String,
    pub colorinvertable: bool,
    pub contenttype: String,
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
pub struct State {
    pub name: String,
    pub input: String,
}
