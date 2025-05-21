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

use js_int::uint;
use matrix_sdk::attachment::{AttachmentConfig, AttachmentInfo, BaseImageInfo};

use reqwest::{ClientBuilder, redirect};
use tempfile::Builder;
use tokio::time::{Duration, interval};
use tokio_postgres::types::Type as dbtype;

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
    /// Database to store persistent
    #[serde(default = "presence_db")]
    pub presence_db: String,
    /// Keywords to respond to for retrieving heatmap. Defaults are `heatmap`, `hm`
    #[serde(default = "keywords_heatmap")]
    pub keywords_heatmap: Vec<String>,
    /// External heatmap url map
    pub heatmap_map: HashMap<String, String>,
    /// Directory where heatmap temp data will be written to. Default is `./`
    #[serde(default = "heatmap_tmp_dir")]
    pub heatmap_tmp_dir: String,
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

fn presence_db() -> String {
    "notbot".s()
}

fn keywords_heatmap() -> Vec<String> {
    vec!["heatmap".s(), "hm".s()]
}

fn heatmap_tmp_dir() -> String {
    "./".s()
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
    at.spawn(rx, module_config.clone(), at_processor);

    let heatmap = ModuleInfo::new(
        "heatmap",
        "show activity at the hackerspace for the last week",
        vec![],
        TriggerType::Keyword(module_config.keywords_heatmap.clone()),
        Some("error generating heatmap"),
        module_config,
        heatmap,
    );

    Ok(vec![at, heatmap])
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

/// Present a heatmap of people activity at the Hackerspace
///
/// Currently fetches external data, but once we gather enough of our own, we should switch to that
pub async fn heatmap(event: ConsumerEvent, config: ModuleConfig) -> anyhow::Result<()> {
    let name = room_name(&event.room);

    let period: HeatmapPeriod = event.args.try_into()?;

    let base_url = match config.heatmap_map.get(&name) {
        Some(url) => url,
        None => bail!("no heatmap source defined for this channel"),
    };

    let url = format!("{base_url}&period={}", Into::<String>::into(period.clone()));

    let client = ClientBuilder::new()
        .redirect(redirect::Policy::none())
        .build()?;

    let response = client.get(url).send().await?;

    if !response.status().is_success() {
        bail!("wrong api response: {:?}", response.status());
    };

    let data: heatmap_api::Root = response.json().await?;

    let named_tempfile = Builder::new()
        .prefix("notbot-heatmap-")
        .suffix(".png")
        .rand_bytes(5)
        .tempfile_in(config.heatmap_tmp_dir)?;

    let name = named_tempfile
        .path()
        .to_str()
        .ok_or(anyhow::anyhow!("tempfile error"))?;

    trace!("heatmap tmpfile: {name}");
    heatmap_api::draw(name, data.data)?;

    let image = fs::read(name)?;

    let caption = match period {
        HeatmapPeriod::All => Some("All acquired datapoints".s()),
        _ => Some(format!("Activity in the last {period:?}")),
    };

    let attachment_config = AttachmentConfig::new()
        .caption(caption)
        .info(AttachmentInfo::Image(BaseImageInfo {
            height: Some(uint!(330)),
            width: Some(uint!(1010)),
            ..Default::default()
        }));

    event
        .room
        .send_attachment("heatmap.png", &mime::IMAGE_PNG, image, attachment_config)
        .await?;

    std::fs::remove_file(name)?;

    Ok(())
}

#[derive(Debug, Clone)]
enum HeatmapPeriod {
    Week,
    Month,
    Year,
    All,
}

impl TryFrom<Option<String>> for HeatmapPeriod {
    type Error = anyhow::Error;

    fn try_from(s: Option<String>) -> anyhow::Result<HeatmapPeriod> {
        use HeatmapPeriod::*;

        match s {
            None => Ok(Week),
            Some(maybe) => match maybe.to_lowercase().trim() {
                "week" | "tydzień" | "tydzien" => Ok(Week),
                "month" | "miesiąc" | "miesiac" => Ok(Month),
                "year" | "rok" => Ok(Year),
                "all" | "wszystko" => Ok(All),
                _ => bail!("invalid period"),
            },
        }
    }
}

impl From<HeatmapPeriod> for String {
    fn from(val: HeatmapPeriod) -> Self {
        use HeatmapPeriod::*;
        match val {
            Week => "week".s(),
            Month => "month".s(),
            Year => "year".s(),
            All => "all".s(),
        }
    }
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

#[derive(Clone, Debug)]
struct PresenceData {
    handle: String,
}

impl PresenceData {
    const INSERT_DATAPOINT: &str = r#"INSERT INTO presence (ts, url, count)
    VALUES ( now(), $1, $2 )"#;
    async fn insert(self, url: String, count: i64) -> anyhow::Result<()> {
        let client = DBPools::get_client(self.handle.as_str()).await?;
        let statement = client
            .prepare_typed_cached(Self::INSERT_DATAPOINT, &[dbtype::VARCHAR, dbtype::INT8])
            .await?;
        client.execute(&statement, &[&url, &count]).await?;
        Ok(())
    }
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
            let presence = PresenceData {
                handle: module_config.presence_db.clone(),
            };

            trace!("fetching spaceapi url: {}", url);
            let data = match fetch_and_decode_json::<space_api::SpaceAPI>(url.to_owned()).await {
                Ok(d) => d,
                Err(fe) => {
                    error!("error fetching data: {fe}");
                    if let Err(e) = presence.insert(url.clone(), -1).await {
                        error!("error storing spaceapi persistence data: {e}");
                    };
                    continue;
                }
            };

            let current: Vec<String> = names_dehighlighted(data.sensors.people_now_present);

            if let Err(e) = presence.insert(url.clone(), current.len() as i64).await {
                error!("error storing spaceapi persistence data: {e}");
            };

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
    #[derive(Clone, Deserialize, Debug)]
    pub struct PeopleNowPresent {
        pub value: u32,
        pub names: Vec<String>,
    }
}

pub mod heatmap_api {
    //! Module for making interactions with the heatmap API, including rendering data it returns, easier.
    use plotters::{prelude::*, style::full_palette::PURPLE_A400};
    use serde_derive::Deserialize;
    use serde_nested_with::serde_nested;
    use serde_this_or_that::as_f64;

    #[allow(dead_code)]
    #[derive(Default, Debug, Clone, Deserialize)]
    pub(crate) struct Root {
        pub copyright: String,
        pub license: String,
        pub data: Data,
        pub tz: String,
        pub period: String,
        #[serde(rename = "space-state")]
        pub space_state: String,
    }

    /// Structure reflecting API response, somewhat. The floats are a lie. The api actually returns a list of strings holding floats, with
    /// a single actual float at the end.
    #[allow(dead_code)]
    #[serde_nested]
    #[derive(Default, Debug, Clone, Deserialize)]
    pub struct Data {
        /// As the name `n2` (or, in the source json, just `2`) implies, the first day of the week - Monday datapoints
        #[serde(rename = "2")]
        #[serde_nested(sub = "f64", serde(deserialize_with = "as_f64"))]
        pub n2: Vec<f64>,
        /// Tuesday
        #[serde(rename = "3")]
        #[serde_nested(sub = "f64", serde(deserialize_with = "as_f64"))]
        pub n3: Vec<f64>,
        /// Wednesday
        #[serde(rename = "4")]
        #[serde_nested(sub = "f64", serde(deserialize_with = "as_f64"))]
        pub n4: Vec<f64>,
        /// Thursday
        #[serde(rename = "5")]
        #[serde_nested(sub = "f64", serde(deserialize_with = "as_f64"))]
        pub n5: Vec<f64>,
        /// Friday
        #[serde(rename = "6")]
        #[serde_nested(sub = "f64", serde(deserialize_with = "as_f64"))]
        pub n6: Vec<f64>,
        /// Saturday
        #[serde(rename = "7")]
        #[serde_nested(sub = "f64", serde(deserialize_with = "as_f64"))]
        pub n7: Vec<f64>,
        /// Sunday
        #[serde(rename = "1")]
        #[serde_nested(sub = "f64", serde(deserialize_with = "as_f64"))]
        pub n1: Vec<f64>,
        /// Overall averages. Not used.
        pub avg: Vec<f64>,
    }

    /// Renders the provided [`Data`] into a file. Formatting options are, for now, hardcoded.
    pub fn draw(out_file: &str, data: Data) -> anyhow::Result<()> {
        let colormap: Box<dyn ColorMap<RGBAColor>> = Box::new(ViridisRGBA {});

        // 24*7 * 40×40 + legend + 5/5/5/5 margins
        let root = BitMapBackend::new(out_file, (1010, 330))
            .into_drawing_area()
            .margin(5, 5, 5, 5);
        root.fill(&TRANSPARENT)?;

        let text_style = TextStyle::from(("monospace", 20).into_font());

        let hour_areas = root.margin(0, 970, 40, 0).split_evenly((1, 24));
        for (i, area) in hour_areas.iter().enumerate() {
            let text_zero = format!("{:0>2}", i);
            let text = format!("{:>3}", text_zero);
            area.draw_text(&text, &text_style.color(&PURPLE_A400), (5, 13))?;
        }

        let day_areas = root.margin(40, 0, 0, 270).split_evenly((7, 1));
        for (i, area) in day_areas.iter().enumerate() {
            let day: Weekday = i.into();
            let text = format!("{:?}", day);
            area.draw_text(&text, &text_style.color(&PURPLE_A400), (5, 13))?;
        }

        let drawing_areas = root.margin(40, 0, 40, 0).split_evenly((7, 24));
        let mut areas = drawing_areas.into_iter();

        // monday is n2? ok…
        for datapoint in skip_last(data.n2.iter())
            .chain(skip_last(data.n3.iter()))
            .chain(skip_last(data.n4.iter()))
            .chain(skip_last(data.n5.iter()))
            .chain(skip_last(data.n6.iter()))
            .chain(skip_last(data.n7.iter()))
            .chain(skip_last(data.n1.iter()))
        {
            let area = areas.next().ok_or(anyhow::anyhow!("not enough areas?"))?;
            let value = format!("{:>3}", (datapoint * 100.0).round());
            let bg_color = colormap.get_color(datapoint.to_owned() as f32);

            // https://gamedev.stackexchange.com/a/38561
            let (c_red, c_green, c_blue) = bg_color.rgb();
            let c_red: f64 = Into::<f64>::into(c_red) / 255.0;
            let c_green: f64 = Into::<f64>::into(c_green) / 255.0;
            let c_blue: f64 = Into::<f64>::into(c_blue) / 255.0;
            const GAMMA: f64 = 2.2;
            let l: f64 = 0.2126 * c_red.powf(GAMMA)
                + 0.7152 * c_green.powf(GAMMA)
                + 0.0722 * c_blue.powf(GAMMA);

            let text_color = if l > 0.5_f64.powf(GAMMA) {
                &BLACK
            } else {
                &WHITE
            };

            area.margin(2, 2, 2, 2).fill(&bg_color)?;
            area.draw_text(&value, &text_style.color(text_color), (5, 13))?;
        }
        Ok(())
    }

    #[derive(Debug)]
    enum Weekday {
        Mon,
        Tue,
        Wed,
        Thu,
        Fri,
        Sat,
        Sun,
    }

    impl From<usize> for Weekday {
        fn from(value: usize) -> Self {
            use Weekday::*;
            match value {
                0 => Mon,
                1 => Tue,
                2 => Wed,
                3 => Thu,
                4 => Fri,
                5 => Sat,
                6 => Sun,
                _ => Mon,
            }
        }
    }

    fn skip_last<I, T>(iter: I) -> impl Iterator<Item = T>
    where
        I: Iterator<Item = T> + std::iter::DoubleEndedIterator + std::iter::ExactSizeIterator,
    {
        iter.rev().skip(1).rev()
    }
}
