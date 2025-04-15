use core::{error::Error as StdError, fmt};
use std::convert::TryFrom;
use std::sync::{Arc, Mutex};
use std::{fs, io};

use serde::Deserialize;
use toml::{Table, Value};

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    NoPath(String),
    NoModuleConfig(String),
}

impl StdError for ConfigError {}

impl From<io::Error> for ConfigError {
    fn from(e: io::Error) -> Self {
        ConfigError::Io(e)
    }
}

impl From<toml::de::Error> for ConfigError {
    fn from(e: toml::de::Error) -> Self {
        ConfigError::Parse(e)
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ConfigError::Io(e) => write!(fmt, "IO error: {}", e),
            ConfigError::Parse(e) => write!(fmt, "parsing error: {}", e),
            ConfigError::NoPath(e) => write!(fmt, "No config path provided: {}", e),
            ConfigError::NoModuleConfig(e) => write!(fmt, "No configuration for module: {}", e),
        }
    }
}

pub trait ModuleConfig: fmt::Debug + Clone + Send {}

#[derive(Deserialize, Debug, Clone)]
struct ConfigInner {
    homeserver: String,
    user_id: String,
    password: String,
    data_dir: String,
    device_id: String,
    #[allow(dead_code)]
    module: Table,
}

impl TryFrom<String> for ConfigInner {
    type Error = ConfigError;
    fn try_from(path: String) -> Result<Self, Self::Error> {
        let config_content = fs::read_to_string(&path)?;
        Ok(toml::from_str::<ConfigInner>(&config_content)?)
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    inner: Arc<Mutex<ConfigInner>>,
}

impl TryFrom<String> for Config {
    type Error = ConfigError;
    fn try_from(path: String) -> Result<Self, Self::Error> {
        Ok(Config {
            inner: Arc::new(Mutex::new(path.try_into()?)),
        })
    }
}

impl TryFrom<Option<String>> for Config {
    type Error = ConfigError;
    fn try_from(maybe_path: Option<String>) -> Result<Self, Self::Error> {
        let path = match maybe_path {
            Some(p) => p,
            None => return Err(ConfigError::NoPath("no path provided".to_string())),
        };

        Ok(path.try_into()?)
    }
}

impl Config {
    pub fn new(path: String) -> anyhow::Result<Self> {
        Ok(path.try_into()?)
    }

    #[allow(dead_code)]
    pub fn reload(mut self, path: String) -> anyhow::Result<Config> {
        let new_cfg = Arc::new(Mutex::new(TryInto::<ConfigInner>::try_into(path)?));
        self.inner = new_cfg;
        Ok(self)
    }

    pub fn homeserver(&self) -> String {
        let inner = &self.inner.lock().unwrap();
        inner.homeserver.clone()
    }

    pub fn user_id(&self) -> String {
        let inner = &self.inner.lock().unwrap();
        inner.user_id.clone()
    }

    pub fn password(&self) -> String {
        let inner = &self.inner.lock().unwrap();
        inner.password.clone()
    }

    pub fn data_dir(&self) -> String {
        let inner = &self.inner.lock().unwrap();
        inner.data_dir.clone()
    }

    pub fn device_id(&self) -> String {
        let inner = &self.inner.lock().unwrap();
        inner.device_id.clone()
    }

    pub(crate) fn module_config_value(&self, n: &str) -> Result<Value, ConfigError> {
        let inner = &self.inner.lock().unwrap();
        if !inner.module.contains_key(n) {
            return Err(ConfigError::NoModuleConfig(n.to_owned()));
        };

        Ok(inner.module[n].clone())
    }
}
