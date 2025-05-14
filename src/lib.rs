#![warn(missing_docs)]
#![doc = include_str!("../README.md")]

pub mod prelude;

pub mod alerts;
pub mod autojoiner;
pub mod botmanager;
pub mod config;
pub mod db;
pub mod forgejo;
pub mod gerrit;
pub mod inviter;
pub mod kasownik;
pub mod klaczdb;
pub mod metrics;
pub mod module;
pub mod notbottime;
pub mod notmun;
mod sage;
pub mod spaceapi;
pub mod tools;
pub mod webterface;
pub mod wolfram;
