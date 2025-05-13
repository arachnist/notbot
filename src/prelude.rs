//! Re-export of functions/macros/modules commonly used for module development

pub use crate::config::Config;

pub use crate::db::DBPools;

pub use crate::module::{
    Acl, ConsumerEvent, Consumption, ModuleInfo, PassThroughModuleInfo, TriggerType, WorkerInfo,
};
pub use crate::webterface::{HswawAdditionalClaims, WebAppState};

pub use crate::tools::*;

pub use std::collections::HashMap;
pub use std::convert::{From, TryFrom};
pub use std::str::FromStr;
pub use std::sync::{Arc, LazyLock, Mutex};
pub use std::time::{Duration, Instant, SystemTime};
pub use std::{fs, io, path::Path};

pub use core::{error::Error as StdError, fmt};

pub use anyhow::{anyhow, bail};

pub use matrix_sdk::event_handler::{Ctx, EventHandlerHandle};
pub use matrix_sdk::ruma::events::room::{
    member::{MembershipChange, RoomMemberEvent, RoomMemberEventContent, StrippedRoomMemberEvent},
    message::{MessageType, RoomMessageEventContent},
};
pub use matrix_sdk::ruma::events::{
    AnyStateEvent, AnySyncTimelineEvent, AnyTimelineEvent, Mentions,
};
pub use matrix_sdk::ruma::{OwnedEventId, OwnedRoomAliasId, OwnedRoomId, OwnedUserId, UserId};
pub use matrix_sdk::{Client, Room};

pub use tokio::sync::mpsc;

pub use tracing::{debug, error, info, trace, warn};

pub use serde::de;
pub use serde_derive::{Deserialize, Serialize};

// hack
pub use notbot_axum_oidc as axum_oidc;
