use crate::prelude::*;

use serde_json::Value;

use urlencoding::encode as uencode;

use prometheus::Counter;
use prometheus::{opts, register_counter};

lazy_static! {
    static ref WOLFRAM_CALLS: Counter = register_counter!(opts!(
        "wolfram_calls_total",
        "Number of times wolfram was called",
    ))
    .unwrap();
    static ref WOLFRAM_CALLS_SUCCESSFUL: Counter = register_counter!(opts!(
        "wolfram_calls_total",
        "Number of times wolfram was called",
    ))
    .unwrap();
}

#[derive(Clone, Deserialize)]
pub struct ModuleConfig {
    pub app_id: String,
}

pub(crate) fn modules() -> Vec<ModuleStarter> {
    vec![(module_path!(), module_starter)]
}

fn module_starter(client: &Client, config: &Config) -> anyhow::Result<EventHandlerHandle> {
    let command_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
    Ok(client.add_event_handler(move |ev, room| {
        simple_command_wrapper(ev, room, command_config, vec![".c".to_string()], wolfram)
    }))
}

async fn wolfram(
    _room: Room,
    _sender: OwnedUserId,
    _keyword: String,
    argv: Vec<String>,
    config: ModuleConfig,
) -> anyhow::Result<String> {
    WOLFRAM_CALLS.inc();

    let text_query = argv.join(" ");
    let query = uencode(text_query.as_str());

    let url: String = "http://api.wolframalpha.com/v2/query?input=".to_owned()
        + query.as_ref()
        + "&appid="
        + config.app_id.as_str()
        + "&output=json";

    let Ok(data) = fetch_and_decode_json::<WolframAlpha>(url).await else {
        return Err(anyhow::Error::msg("couldn't fetch data from wolfram"));
    };

    if !data.queryresult.success || data.queryresult.numpods == 0 {
        return Ok("no results".to_string());
    };

    let mut response_parts: Vec<String> = vec![];

    for pod in data.queryresult.pods {
        if pod.primary.is_some_and(|x| x) {
            response_parts.push(pod.title + ": " + pod.subpods[0].plaintext.as_str());
        }
    }

    Ok(response_parts.join("\n"))
}

#[derive(Clone, Deserialize)]
pub struct WolframAlpha {
    pub queryresult: Queryresult,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize)]
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

#[allow(dead_code)]
#[derive(Clone, Deserialize)]
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

#[allow(dead_code)]
#[derive(Clone, Deserialize)]
pub struct Subpod {
    pub title: String,
    pub img: Img,
    pub plaintext: String,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize)]
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

#[allow(dead_code)]
#[derive(Clone, Deserialize)]
pub struct State {
    pub name: String,
    pub input: String,
}
