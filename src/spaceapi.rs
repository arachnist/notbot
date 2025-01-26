use reqwest::blocking::Client as Client;

use serde_derive::Deserialize;
use serde_derive::Serialize;

fn lmao() {
    // tracing_subscriber::fmt::init();
    let client = Client::new();
    let resp = client.get("https://hackerspace.pl/spaceapi").send().unwrap();
    let spacepi_json = resp.json::<Root>().unwrap();

    print!("parsed json: {:?}", spacepi_json)
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Root {
    pub api: String,
    pub space: String,
    pub logo: String,
    pub url: String,
    pub location: Location,
    pub state: State,
    pub contact: Contact,
    #[serde(rename = "issue_report_channels")]
    pub issue_report_channels: Vec<String>,
    pub projects: Vec<String>,
    pub feeds: Feeds,
    pub sensors: Sensors,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    pub lat: f64,
    pub lon: f64,
    pub address: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub open: bool,
    pub message: String,
    pub icon: Icon,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Icon {
    pub open: String,
    pub closed: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    pub facebook: String,
    pub irc: String,
    pub ml: String,
    pub twitter: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Feeds {
    pub blog: Blog,
    pub calendar: Calendar,
    pub wiki: Wiki,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Blog {
    #[serde(rename = "type")]
    pub type_field: String,
    pub url: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    #[serde(rename = "type")]
    pub type_field: String,
    pub url: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Wiki {
    #[serde(rename = "type")]
    pub type_field: String,
    pub url: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sensors {
    #[serde(rename = "people_now_present")]
    pub people_now_present: Vec<PeopleNowPresent>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeopleNowPresent {
    pub value: u16,
    pub names: Vec<String>,
}
