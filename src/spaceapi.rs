//! Query SpaceAPI endpoints and observe membership changes.
//!
//! Retrieves and monitors members presence at the hackerspace using a compatible [SpaceAPI](https://spaceapi.io/)
//!
//! # Configuration
//!
//! [`ModuleConfig`]
//!
//! ```toml
//! [module."notbot::spaceapi"]
//! presence_interval = 30
//! empty_response = "Nikdo není doma..."
//! # keywords = [ "at" ]
//!
//! [module."notbot::spaceapi".presence_map]
//! "https://hackerspace.pl/spaceapi" = [
//!     "#members:hackerspace.pl",
//!     "#notbot-test-private-room:is-a.cat",
//!     "#bottest:is-a.cat"
//! ]
//!
//! [module."notbot::spaceapi".room_map]
//! "default" = "https://hackerspace.pl/spaceapi"
//! "#members" = "https://hackerspace.pl/spaceapi"
//! ```
//!
//! # Usage
//!
//! Keywords:
//! * `at` - [`at_processor`] - return the list of names listed as present by SpaceAPI endpoint set for current room.
//!
//! Passive presence updates: [`presence_observer`]
//! ```chat logs
//! Notice(notbot) -> #members:hackerspace.pl: arrived: foo, also there: bar, baz
//! Notice(notbot) -> #members:hackerspace.pl: left: foo, still there: bar, baz
//! Notice(notbot) -> #members:hackerspace.pl: left: bar, still there: baz
//! ```

use crate::prelude::*;

use tokio::time::{Duration, interval};

/// SpaceAPI module configuration
#[derive(Clone, Deserialize)]
pub struct ModuleConfig {
    /// Room to SpaceAPI endpoint map. special value "default" will be used when a room specific endpoint.
    pub room_map: HashMap<String, String>,
    /// Map of SpaceAPI endpoints to a list of rooms.
    pub presence_map: HashMap<String, Vec<String>>,
    /// How often should the presence observer check for presence changes, in seconds. Default is 30.
    #[serde(default = "presence_interval")]
    pub presence_interval: u64,
    /// String to reply with when queried for currently present members, and no members are present. Default is `Nikdo není doma...`
    #[serde(default = "empty_response")]
    pub empty_response: String,
    /// Keywords to respond to for checking currently present members. Default is `at`
    #[serde(default = "keywords")]
    pub keywords: Vec<String>,
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
    at.spawn(rx, module_config, at_processor);

    Ok(vec![at])
}

/// query SpaceAPI for present state.
pub async fn at_processor(event: ConsumerEvent, config: ModuleConfig) -> anyhow::Result<()> {
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

/// Worker observing SpaceAPI endpoints for changes and providing updates to configured rooms.
pub async fn presence_observer(client: Client, module_config: ModuleConfig) -> anyhow::Result<()> {
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

/// Insert `\u{200B}` - unicode Zero Width Space - in between the first and remaining characters
/// of present people names, to avoid unnecessary mentions in the chat Room of people present.
///
/// Primarily used to avoid mentions on channels bridged with IRC, but Matrix clients can also
/// send a notification on strings matching the configured displayname. Some Matrix clients,
/// in particular, old Element versions for apple mobile devices, did render this as `&zwsp` in
/// the middle of names, making the notifications less readable for them, so it can be a trade-off
/// in certain situations.
pub fn names_dehighlighted(present: Vec<space_api::PeopleNowPresent>) -> Vec<String> {
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
