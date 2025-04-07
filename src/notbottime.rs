use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd)]
pub struct NotBotTime(pub SystemTime);
const NOTBOT_EPOCH: NotBotTime = NotBotTime(UNIX_EPOCH);

impl NotBotTime {
    pub fn now() -> Self {
        NotBotTime(SystemTime::now())
    }
}

impl From<Vec<u8>> for NotBotTime {
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
    fn from(s: NotBotTime) -> Vec<u8> {
        let d: Duration = match s.0.duration_since(UNIX_EPOCH) {
            Ok(d) => d,
            Err(_) => Duration::from_secs(0),
        };

        d.as_secs().to_le_bytes().to_vec()
    }
}
