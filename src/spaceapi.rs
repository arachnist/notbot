use crate::prelude::*;

use tokio::task::AbortHandle;

use tokio::time::{interval, Duration};

use lazy_static::lazy_static;
use prometheus::Counter;
use prometheus::{opts, register_counter};

lazy_static! {
    static ref CHECKINATOR_CALLS: Counter = register_counter!(opts!(
        "checkinator_calls_total",
        "Number of times checkinator was called",
    ))
    .unwrap();
}

pub(crate) fn workers() -> Vec<WorkerStarter> {
    vec![(module_path!(), worker_starter)]
}

pub(crate) fn starter(_: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering modules");
    let mut modules: Vec<ModuleInfo> = vec![];

    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;

    let (tx, rx) = mpsc::channel::<ConsumerEvent>(1);
    tokio::task::spawn(at_consumer(rx, module_config));
    let at = ModuleInfo {
        name: "at".s(),
        help: "see who is present at the hackerspace".s(),
        acl: vec![],
        trigger: TriggerType::Keyword(vec!["at".s()]),
        channel: Some(tx),
    };
    modules.push(at);

    Ok(modules)
}

async fn at_consumer(
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

        if let Err(e) = at(event.clone(), config.clone()).await {
            if let Err(e) = event
                .room
                .send(RoomMessageEventContent::text_plain(format!(
                    "error getting presence status: {e}"
                )))
                .await
            {
                error!("error while sending presence status: {e}");
            };
        }
    }
}

async fn at(event: ConsumerEvent, config: ModuleConfig) -> anyhow::Result<()> {
    let name = room_name(&event.room);

    let url = match config.room_map.get(&name) {
        Some(url) => url,
        None => {
            debug!("no spaceapi url found, using default");
            match config.room_map.get("default") {
                None => bail!("no spaceapi url found"),
                Some(u) => u,
            }
        }
    };

    let data = fetch_and_decode_json::<SpaceAPI>(url.to_owned()).await?;
    let present: Vec<String> = names_dehighlighted(data.sensors.people_now_present);

    let response = if present.is_empty() {
        config.empty_response.clone()
    } else {
        present.join(", ")
    };

    event
        .room
        .send(RoomMessageEventContent::text_plain(response))
        .await?;
    Ok(())
}

#[derive(Clone, Deserialize)]
pub struct ModuleConfig {
    room_map: HashMap<String, String>,
    presence_map: HashMap<String, Vec<String>>,
    presence_interval: u64,
    empty_response: String,
}

fn worker_starter(client: &Client, config: &Config) -> anyhow::Result<AbortHandle> {
    let module_config: ModuleConfig = config.module_config_value(module_path!())?.try_into()?;
    let worker = tokio::task::spawn(presence_observer(client.clone(), module_config));
    Ok(worker.abort_handle())
}

async fn presence_observer(client: Client, module_config: ModuleConfig) {
    let mut interval = interval(Duration::from_secs(module_config.presence_interval));
    let mut present: HashMap<String, Vec<String>> = Default::default();
    let mut first_loop: HashMap<String, bool> = Default::default();

    for url in module_config.presence_map.keys() {
        present.insert(url.clone(), vec![]);
        first_loop.insert(url.clone(), true);
    }

    loop {
        interval.tick().await;

        for (url, rooms) in &module_config.presence_map {
            trace!("fetching spaceapi url: {}", url);
            let data = match fetch_and_decode_json::<SpaceAPI>(url.to_owned()).await {
                Ok(d) => d,
                Err(fe) => {
                    error!("error fetching data: {fe}");
                    continue;
                }
            };

            let current: Vec<String> = names_dehighlighted(data.sensors.people_now_present);
            let mut arrived: Vec<String> = vec![];
            let mut left: Vec<String> = vec![];
            let mut also_there: Vec<String> = vec![];

            for name in &current {
                if !present[url].contains(name) {
                    arrived.push(name.clone());
                };
            }

            for name in &present[url] {
                if current.contains(name) {
                    also_there.push(name.clone());
                } else {
                    left.push(name.clone());
                };
            }

            present.insert(url.clone(), current);

            if first_loop[url] {
                first_loop.insert(url.clone(), false);
                continue;
            }

            let mut response_parts: Vec<String> = vec![];

            if !arrived.is_empty() {
                response_parts.push(["arrived: ", &arrived.join(", ")].concat());
            };

            if !left.is_empty() {
                response_parts.push(["left: ", &left.join(", ")].concat());
            };

            if !also_there.is_empty() {
                // When people have both left and arrived, using the "also" form makes more
                // grammatical sense, hence the priority decoding below.
                let qualifier = if !arrived.is_empty() { "also" } else { "still" };
                response_parts.push([qualifier, " there: ", &also_there.join(", ")].concat());
            };

            if arrived.is_empty() && left.is_empty() {
                continue;
            };

            let response = RoomMessageEventContent::notice_plain(response_parts.join(", "));
            for maybe_room in rooms {
                let room_id: OwnedRoomId = match maybe_room.clone().try_into() {
                    Ok(r) => r,
                    Err(_) => {
                        let alias_id = match OwnedRoomAliasId::try_from(maybe_room.clone()) {
                            Ok(a) => a,
                            Err(_) => {
                                error!("couldn't parse room name: {}", maybe_room);
                                continue;
                            }
                        };

                        match client.resolve_room_alias(&alias_id).await {
                            Err(e) => {
                                error!("couldn't resolve room alias: {e}");
                                continue;
                            }
                            Ok(r) => r.room_id,
                        }
                    }
                };

                let room = match client.get_room(&room_id) {
                    Some(r) => r,
                    None => continue,
                };

                if let Err(e) = room.send(response.clone()).await {
                    error!("error while sending presence update: {e}");
                };
            }
        }
    }
}

fn names_dehighlighted(present: Vec<PeopleNowPresent>) -> Vec<String> {
    let mut dehighlighted: Vec<String> = vec![];

    for sensor in present {
        for name in sensor.names {
            let mut chars = name.chars();
            dehighlighted.push(match chars.next() {
                None => String::new(),
                Some(first) => first.to_string() + "\u{200B}" + chars.as_str(),
            });
        }
    }

    dehighlighted
}

#[allow(dead_code)]
#[derive(Clone, Deserialize)]
pub struct SpaceAPI {
    pub api_compatibility: Vec<String>,
    pub space: String,
    pub logo: String,
    pub url: String,
    pub location: Location,
    pub state: State,
    pub contact: Contact,
    pub projects: Vec<String>,
    pub feeds: Feeds,
    pub sensors: Sensors,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize)]
pub struct Location {
    pub lat: f64,
    pub lon: f64,
    pub address: String,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize)]
pub struct State {
    pub open: bool,
    pub message: String,
    pub icon: Icon,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize)]
pub struct Icon {
    pub open: String,
    pub closed: String,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize)]
pub struct Contact {
    pub facebook: String,
    pub irc: String,
    pub mastodon: String,
    pub matrix: String,
    pub ml: String,
    pub twitter: String,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize)]
pub struct Feeds {
    pub blog: Blog,
    pub calendar: Calendar,
    pub wiki: Wiki,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize)]
pub struct Blog {
    #[serde(rename = "type")]
    pub type_field: String,
    pub url: String,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize)]
pub struct Calendar {
    #[serde(rename = "type")]
    pub type_field: String,
    pub url: String,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize)]
pub struct Wiki {
    #[serde(rename = "type")]
    pub type_field: String,
    pub url: String,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize)]
pub struct Sensors {
    pub people_now_present: Vec<PeopleNowPresent>,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize)]
pub struct PeopleNowPresent {
    pub value: u32,
    pub names: Vec<String>,
}
