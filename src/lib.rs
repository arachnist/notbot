#![warn(missing_docs)]

mod alerts;
mod botmanager;

mod prelude;

pub use crate::botmanager::BotManager;
pub mod config;
pub mod db;
pub mod klaczdb;
pub mod metrics;
pub mod module;
pub mod notmun;
pub mod tools;
pub mod webterface;

mod autojoiner;
pub mod inviter;
pub mod kasownik;
mod notbottime;
mod sage;
pub mod spaceapi;
pub mod wolfram;
