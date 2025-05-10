#![warn(missing_docs)]

mod alerts;
mod botmanager;
mod metrics;

pub use crate::botmanager::BotManager;
pub mod config;
pub mod db;
pub mod klaczdb;
pub mod module;
pub mod prelude;
pub mod tools;
pub mod webterface;

mod autojoiner;
mod inviter;
mod kasownik;
mod notbottime;
mod notmun;
mod sage;
mod spaceapi;
pub mod wolfram;
