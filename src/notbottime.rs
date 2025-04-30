use crate::prelude::*;

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd)]
pub(crate) struct NotBotTime(pub SystemTime);
pub(crate) const NOTBOT_EPOCH: NotBotTime = NotBotTime(UNIX_EPOCH);

impl NotBotTime {
    pub(crate) fn now() -> Self {
        NotBotTime(SystemTime::now())
    }

    pub(crate) fn duration_since(&self, earlier: NotBotTime) -> Result<Duration, SystemTimeError> {
        self.0.duration_since(earlier.0)
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
        let d: Duration = match s.duration_since(NOTBOT_EPOCH) {
            Ok(d) => d,
            Err(_) => Duration::from_secs(0),
        };

        d.as_secs().to_le_bytes().to_vec()
    }
}
