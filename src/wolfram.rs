use crate::prelude::*;

use serde_json::Value;

use urlencoding::encode as uencode;

#[derive(Clone, Deserialize)]
pub struct ModuleConfig {
    pub app_id: String,
}

pub(crate) fn starter(mx: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering modules");
    let mut modules: Vec<ModuleInfo> = vec![];

    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;

    let (tx, rx) = mpsc::channel::<ConsumerEvent>(1);
    tokio::task::spawn(consumer(rx, module_config));
    let at = ModuleInfo {
        name: "wolfram".s(),
        help: "calculate something using wolfram alpha".s(),
        acl: vec![],
        trigger: TriggerType::Keyword(vec!["c".s(), "wolfram".s()]),
        channel: Some(tx),
    };
    modules.push(at);

    Ok(modules)
}

async fn consumer(
    mut rx: mpsc::Receiver<ConsumerEvent>,
    config: ModuleConfig,
) -> anyhow::Result<()> {
    loop {
        let event = match rx.recv().await {
            Some(e) => e,
            None => {
                error!("channel closed, goodbye! :(");
                bail!("channel closed");
            }
        };

        if let Err(e) = processor(event.clone(), config.clone()).await {
            if let Err(e) = event
                .room
                .send(RoomMessageEventContent::text_plain(format!(
                    "error getting wolfram response: {e}"
                )))
                .await
            {
                error!("error while sending wolfram response: {e}");
            };
        }
    }
}

async fn processor(event: ConsumerEvent, config: ModuleConfig) -> anyhow::Result<()> {
    let Some(text_query) = event.args else {
        event
            .room
            .send(RoomMessageEventContent::text_plain(
                "missing argument: query",
            ))
            .await?;
        return Ok(());
    };

    let query = uencode(text_query.as_str());

    let url: String = "http://api.wolframalpha.com/v2/query?input=".to_owned()
        + query.as_ref()
        + "&appid="
        + config.app_id.as_str()
        + "&output=json";

    let Ok(data) = fetch_and_decode_json::<WolframAlpha>(url).await else {
        bail!("couldn't fetch data from wolfram")
    };

    if !data.queryresult.success || data.queryresult.numpods == 0 {
        event
            .room
            .send(RoomMessageEventContent::text_plain("no results"))
            .await?;
    };

    let mut response_parts: Vec<String> = vec![];

    for pod in data.queryresult.pods {
        if pod.primary.is_some_and(|x| x) {
            response_parts.push(pod.title + ": " + pod.subpods[0].plaintext.as_str());
        }
    }

    event
        .room
        .send(RoomMessageEventContent::text_plain(
            response_parts.join("\n"),
        ))
        .await?;

    Ok(())
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
