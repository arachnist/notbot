#![warn(missing_docs)]

#![doc = include_str!("../README.md")]

mod prelude;

pub mod alerts;
pub mod autojoiner;
pub mod botmanager;
pub mod config;
pub mod db;
pub mod klaczdb;
pub mod metrics;
pub mod module;
pub mod notmun;
pub mod tools;
pub mod webterface;

pub mod inviter;
pub mod kasownik;
mod notbottime;
mod sage;
pub mod spaceapi;
pub mod wolfram;
