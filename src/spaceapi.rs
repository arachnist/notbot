//! Query SpaceAPI endpoints and observe membership changes.
//!
//! Retrieves and monitors members presence at the hackerspace using a compatible [SpaceAPI](https://spaceapi.io/)
//!
//! # Configuration
//!
//! ```toml
//! # main configuration
//! [module."notbot::spaceapi"]
//! # Unsigned number; optional; how often should SpaceAPI endpoints be checked for presence changes; default: 30
//! presence_interval = 30
//! # String; optional; response used if noone is present; default: "Nikdo není doma..."
//! empty_response = "Nikdo není doma..."
//!
//! # url -> list of rooms map to send presence updates to
//! [module."notbot::spaceapi".presence_map]
//! "https://hackerspace.pl/spaceapi" = [
//!     "#members:hackerspace.pl",
//!     "#notbot-test-private-room:is-a.cat",
//!     "#bottest:is-a.cat"
//! ]
//!
//! # room name -> url map to use when checking presence. special value "default" will be used when a room specific endpoint
//! # is not defined
//! [module."notbot::spaceapi".room_map]
//! "default" = "https://hackerspace.pl/spaceapi"
//! "#members" = "https://hackerspace.pl/spaceapi"
//! ```
//!
//! # Usage
//!
//! Active query of SpaceAPI
//! ```chat logs
//! <ari> .at
//! <notbot> ar
//! ```
//!
//! Passive presence updates
//! ```chat logs
//! Notice(notbot) -> #members:hackerspace.pl: arrived: foo, also there: bar, baz
//! Notice(notbot) -> #members:hackerspace.pl: left: foo, still there: bar, baz
//! Notice(notbot) -> #members:hackerspace.pl: left: bar, still there: baz
//! ```

use crate::prelude::*;

use tokio::time::{interval, Duration};

#[derive(Clone, Deserialize)]
struct ModuleConfig {
    room_map: HashMap<String, String>,
    presence_map: HashMap<String, Vec<String>>,
    #[serde(default = "presence_interval")]
    presence_interval: u64,
    #[serde(default = "empty_response")]
    empty_response: String,
    #[serde(default = "keywords")]
    keywords: Vec<String>,
}

fn presence_interval() -> u64 {
    30
}

fn empty_response() -> String {
    "Nikdo není doma...".s()
}

fn keywords() -> Vec<String> {
    vec!["at".s()]
}

pub(crate) fn starter(_: &Client, config: &Config) -> anyhow::Result<Vec<ModuleInfo>> {
    info!("registering modules");
    let module_config: ModuleConfig = config.typed_module_config(module_path!())?;

    let (tx, rx) = mpsc::channel::<ConsumerEvent>(1);
    let at = ModuleInfo {
        name: "at".s(),
        help: "see who is present at the hackerspace".s(),
        acl: vec![],
        trigger: TriggerType::Keyword(module_config.keywords.clone()),
        channel: tx,
        error_prefix: Some("error getting presence status".s()),
    };
    at.spawn(rx, module_config, processor);

    Ok(vec![at])
}

async fn processor(event: ConsumerEvent, config: ModuleConfig) -> anyhow::Result<()> {
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

    let data = fetch_and_decode_json::<space_api::SpaceAPI>(url.to_owned()).await?;
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

pub(crate) fn workers(mx: &Client, config: &Config) -> anyhow::Result<Vec<WorkerInfo>> {
    let module_config: ModuleConfig = config.typed_module_config(module_path!())?;
    Ok(vec![WorkerInfo::new(
        "observer",
        "observes SpaceAPI endpoints for changes",
        "spaceapi",
        mx.clone(),
        module_config,
        presence_observer,
    )?])
}

async fn presence_observer(client: Client, module_config: ModuleConfig) -> anyhow::Result<()> {
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
            let data = match fetch_and_decode_json::<space_api::SpaceAPI>(url.to_owned()).await {
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

fn names_dehighlighted(present: Vec<space_api::PeopleNowPresent>) -> Vec<String> {
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

pub mod space_api {
    //! Structure for data retrieved from SpaceAPI endpoints.
    //!
    //! Generated from the published json schema, and modified slightly.

    use serde_derive::Deserialize;
    #[allow(dead_code, missing_docs)]
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

    #[allow(dead_code, missing_docs)]
    #[derive(Clone, Deserialize)]
    pub struct Location {
        pub lat: f64,
        pub lon: f64,
        pub address: String,
    }

    #[allow(dead_code, missing_docs)]
    #[derive(Clone, Deserialize)]
    pub struct State {
        pub open: bool,
        pub message: String,
        pub icon: Icon,
    }

    #[allow(dead_code, missing_docs)]
    #[derive(Clone, Deserialize)]
    pub struct Icon {
        pub open: String,
        pub closed: String,
    }

    #[allow(dead_code, missing_docs)]
    #[derive(Clone, Deserialize)]
    pub struct Contact {
        pub facebook: String,
        pub irc: String,
        pub mastodon: String,
        pub matrix: String,
        pub ml: String,
        pub twitter: String,
    }

    #[allow(dead_code, missing_docs)]
    #[derive(Clone, Deserialize)]
    pub struct Feeds {
        pub blog: Blog,
        pub calendar: Calendar,
        pub wiki: Wiki,
    }

    #[allow(dead_code, missing_docs)]
    #[derive(Clone, Deserialize)]
    pub struct Blog {
        #[serde(rename = "type")]
        pub type_field: String,
        pub url: String,
    }

    #[allow(dead_code, missing_docs)]
    #[derive(Clone, Deserialize)]
    pub struct Calendar {
        #[serde(rename = "type")]
        pub type_field: String,
        pub url: String,
    }

    #[allow(dead_code, missing_docs)]
    #[derive(Clone, Deserialize)]
    pub struct Wiki {
        #[serde(rename = "type")]
        pub type_field: String,
        pub url: String,
    }

    #[allow(dead_code, missing_docs)]
    #[derive(Clone, Deserialize)]
    pub struct Sensors {
        pub people_now_present: Vec<PeopleNowPresent>,
    }

    #[allow(dead_code, missing_docs)]
    #[derive(Clone, Deserialize)]
    pub struct PeopleNowPresent {
        pub value: u32,
        pub names: Vec<String>,
    }
}
