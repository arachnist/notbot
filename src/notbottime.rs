//! A wrapper around SystemTime to add (naive) (de)serialization to/from `Vec<u8>`
//!
//! Used for persisting timestamps using [`matrix_sdk::StateStore::set_custom_value`], which only accepts `Vec<u8>` values.
//!
//! TODO: Look into replacing this with serde, or stop using matrix client state store for persistency of custom data.

use std::time::{Duration, SystemTime, SystemTimeError, UNIX_EPOCH};

/// Thin wrapper around [`std::time::SystemTime`]
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd)]
pub struct NotBotTime(pub SystemTime);
/// Thin wrapper around [`std::time::UNIX_EPOCH`]
pub const NOTBOT_EPOCH: NotBotTime = NotBotTime(UNIX_EPOCH);

impl NotBotTime {
    /// Wrapped [`std::time::SystemTime::now`]
    pub fn now() -> Self {
        NotBotTime(SystemTime::now())
    }

    /// Wrapped [`std::time::SystemTime::duration_since`]
    pub fn duration_since(&self, earlier: NotBotTime) -> Result<Duration, SystemTimeError> {
        self.0.duration_since(earlier.0)
    }
}

impl From<Vec<u8>> for NotBotTime {
    /// Naive conversion from a `Vec<u8>` through `u64`
    fn from(v: Vec<u8>) -> Self {
        let boxed_v: Box<[u8; 8]> = match v.try_into() {
            Ok(ba) => ba,
            Err(_) => Box::new([0u8; 8]),
        };
        let d_secs: u64 = u64::from_le_bytes(*boxed_v);
        let d: Duration = Duration::from_secs(d_secs);
        let st: NotBotTime = match NOTBOT_EPOCH.0.checked_add(d) {
            Some(t) => NotBotTime(t),
            None => NotBotTime::now(),
        };

        st
    }
}

impl From<NotBotTime> for Vec<u8> {
    /// Naive conversion from [`NotBotTime`] to `Vec<u8>` through `u64`
    fn from(s: NotBotTime) -> Vec<u8> {
        let d: Duration = match s.duration_since(NOTBOT_EPOCH) {
            Ok(d) => d,
            Err(_) => Duration::from_secs(0),
        };

        d.as_secs().to_le_bytes().to_vec()
    }
}
