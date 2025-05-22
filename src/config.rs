//! Configuration module for the bot. Handles loading the configuration from file, global configuration values, and
//! retrieving module-specific sections.

use crate::prelude::*;

use toml::Table;

/// Known error types that can be returned when (re)loading configuration
#[derive(Debug)]
pub enum ConfigError {
    /// I/O error - file couldn't be (fully) read for whatever reason.
    Io(std::io::Error),
    /// Parsing error - provided configuration file is not valid TOML
    Parse(toml::de::Error),
    /// Requested module section does not exist.
    NoModuleConfig(String),
    /// Module deserializing faile, likely due to missing fields.
    ModuleConfigDeserialize,
    /// Locking inner configuration structure failed
    InnerLockError,
}

impl StdError for ConfigError {}

impl From<io::Error> for ConfigError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<toml::de::Error> for ConfigError {
    fn from(e: toml::de::Error) -> Self {
        Self::Parse(e)
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        use ConfigError::{InnerLockError, Io, ModuleConfigDeserialize, NoModuleConfig, Parse};
        match self {
            Io(e) => write!(fmt, "IO error: {e}"),
            Parse(e) => write!(fmt, "parsing error: {e}"),
            NoModuleConfig(e) => write!(fmt, "No configuration for module: {e}"),
            InnerLockError => write!(fmt, "Locking inner config failed"),
            ModuleConfigDeserialize => write!(fmt, "Module configuration failed deserialization"),
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
struct ConfigInner {
    homeserver: String,
    user_id: String,
    password: String,
    data_dir: String,
    device_id: String,
    #[allow(dead_code)]
    module: Table,
    prefixes: Vec<String>,
    prefixes_restricted: Option<HashMap<String, Vec<String>>>,
    acl_deny: Vec<String>,
    ignored: Vec<String>,
    admins: Vec<String>,
    capacifier_token: String,
    #[serde(default = "empty")]
    modules_disabled: Vec<String>,
    #[serde(default = "empty")]
    modules_fenced: Vec<String>,
}

const fn empty() -> Vec<String> {
    vec![]
}

impl TryFrom<String> for ConfigInner {
    type Error = ConfigError;
    fn try_from(path: String) -> Result<Self, Self::Error> {
        let config_content = fs::read_to_string(&path)?;
        Ok(toml::from_str::<Self>(&config_content)?)
    }
}

/// Object holding bot configuration.
///
/// The only value it holds is an Arc<Mutex<>> to the actual configuration structure.
#[derive(Clone, Debug)]
pub struct Config {
    inner: Arc<Mutex<ConfigInner>>,
}

impl TryFrom<String> for Config {
    type Error = ConfigError;
    fn try_from(path: String) -> Result<Self, Self::Error> {
        Ok(Self {
            inner: Arc::new(Mutex::new(path.try_into()?)),
        })
    }
}

impl Config {
    /// Creates a new Configuration object using the string provided
    ///
    /// # Errors
    /// Will return error if configuration cannot be read or parsed.
    pub fn new(path: String) -> anyhow::Result<Self> {
        Ok(path.try_into()?)
    }

    /// Homeserver the bot is configured to connect to.
    ///
    /// # Panics
    /// Will panic if acquiring mutex on inner configuration structure fails.
    #[must_use]
    pub fn homeserver(&self) -> String {
        let inner = &self.inner.lock().unwrap();
        inner.homeserver.clone()
    }

    /// Bots matrix user id.
    ///
    /// # Panics
    /// Will panic if acquiring mutex on inner configuration structure fails.
    #[must_use]
    pub fn user_id(&self) -> String {
        let inner = &self.inner.lock().unwrap();
        inner.user_id.clone()
    }

    pub(crate) fn password(&self) -> String {
        let inner = &self.inner.lock().unwrap();
        inner.password.clone()
    }

    pub(crate) fn data_dir(&self) -> String {
        let inner = &self.inner.lock().unwrap();
        inner.data_dir.clone()
    }

    pub(crate) fn device_id(&self) -> String {
        let inner = &self.inner.lock().unwrap();
        inner.device_id.clone()
    }

    /// Prefixes bot responds to
    ///
    /// # Panics
    /// Will panic if acquiring mutex on inner configuration structure fails.
    #[must_use]
    pub fn prefixes(&self) -> Vec<String> {
        let inner = &self.inner.lock().unwrap();
        inner.prefixes.clone()
    }

    /// Prefixes bot responds to for certain modules (keyword-only)
    ///
    /// # Panics
    /// Will panic if acquiring mutex on inner configuration structure fails.
    #[must_use]
    pub fn prefixes_restricted(&self) -> Option<HashMap<String, Vec<String>>> {
        let inner = &self.inner.lock().unwrap();
        inner.prefixes_restricted.clone()
    }

    /// Messages to choose from when denying a request.
    ///
    /// # Panics
    /// Will panic if acquiring mutex on inner configuration structure fails.
    #[must_use]
    pub fn acl_deny(&self) -> Vec<String> {
        let inner = &self.inner.lock().unwrap();
        inner.acl_deny.clone()
    }

    /// Known ignored User IDs.
    ///
    /// Not ignored at Matrix (protocol) level (see: [`matrix_sdk::Account::ignore_user`]),
    /// as we still might want to log them.
    ///
    /// # Panics
    /// Will panic if acquiring mutex on inner configuration structure fails.
    #[must_use]
    pub fn ignored(&self) -> Vec<String> {
        let inner = &self.inner.lock().unwrap();
        inner.ignored.clone()
    }

    /// Bot admins.
    ///
    /// # Panics
    /// Will panic if acquiring mutex on inner configuration structure fails.
    #[must_use]
    pub fn admins(&self) -> Vec<String> {
        let inner = &self.inner.lock().unwrap();
        inner.admins.clone()
    }

    /// Capacifier token.
    ///
    /// # Panics
    /// Will panic if acquiring mutex on inner configuration structure fails.
    #[must_use]
    pub fn capacifier_token(&self) -> String {
        let inner = &self.inner.lock().unwrap();
        inner.capacifier_token.clone()
    }

    /// Modules disabled - don't get events passed to them.
    ///
    /// # Panics
    /// Will panic if acquiring mutex on inner configuration structure fails.
    #[must_use]
    pub fn modules_disabled(&self) -> Vec<String> {
        let inner = &self.inner.lock().unwrap();
        inner.modules_disabled.clone()
    }

    /// Modules fenced - get removed from module list on configuration (re)load.
    ///
    /// # Panics
    /// Will panic if acquiring mutex on inner configuration structure fails.
    #[must_use]
    pub fn modules_fenced(&self) -> Vec<String> {
        let inner = &self.inner.lock().unwrap();
        inner.modules_fenced.clone()
    }

    /// Disable a module.
    ///
    /// # Errors
    /// Will return `Err` if acquiring mutex on inner configuration structure fails.
    #[allow(clippy::significant_drop_tightening)]
    pub fn disable_module(&self, name: String) -> anyhow::Result<()> {
        let inner = &mut self
            .inner
            .lock()
            .map_err(|_| anyhow!("acquiring mutex on inner configuration failed"))?;
        if inner.modules_disabled.contains(&name) {
            return Ok(());
        };
        inner.modules_disabled.push(name);
        Ok(())
    }

    /// Fence a module.
    ///
    /// # Errors
    /// Will return `Err` if acquiring mutex on inner configuration structure fails.
    #[allow(clippy::significant_drop_tightening)]
    pub fn fence_module(&self, name: String) -> anyhow::Result<()> {
        let inner = &mut self.inner.lock().map_err(|_| ConfigError::InnerLockError)?;
        if inner.modules_fenced.contains(&name) {
            return Ok(());
        };
        inner.modules_fenced.push(name);
        Ok(())
    }

    /// Enable a module
    ///
    /// # Errors
    /// Will return `Err` if acquiring mutex on inner configuration structure fails.
    pub fn enable_module(&self, name: &str) -> anyhow::Result<()> {
        let () = &mut self
            .inner
            .lock()
            .map_err(|_| ConfigError::InnerLockError)?
            .modules_disabled
            .retain(|x| x != name);
        Ok(())
    }

    /// Unfence a module
    ///
    /// # Errors
    /// Will return `Err` if acquiring mutex on inner configuration structure fails.
    pub fn unfence_module(&self, name: &str) -> anyhow::Result<()> {
        let () = &mut self
            .inner
            .lock()
            .map_err(|_| ConfigError::InnerLockError)?
            .modules_fenced
            .retain(|x| x != name);
        Ok(())
    }

    /// Retrieve module configuration by section name
    ///
    /// # Errors
    /// Will return `Err` if acquiring mutex on inner configuration structure fails,
    /// or deserialization of requested configuration chunk fails.
    pub fn typed_module_config<C>(&self, n: &str) -> Result<C, ConfigError>
    where
        C: de::DeserializeOwned + Clone + Send + Sync + 'static,
    {
        let inner = &self.inner.lock().map_err(|_| ConfigError::InnerLockError)?;
        if !inner.module.contains_key(n) {
            return Err(ConfigError::NoModuleConfig(n.to_owned()));
        };

        inner.module[n]
            .clone()
            .try_into()
            .map_err(|_| ConfigError::ModuleConfigDeserialize)
    }
}
