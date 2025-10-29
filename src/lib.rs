#![warn(
    missing_docs,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::cargo
)]
#![allow(
    clippy::transmute_undefined_repr,
    clippy::transmute_ptr_to_ptr,
    reason = "unavoidable without serde_nested_with changes"
)]
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
pub mod module;
pub mod notmun;
pub mod points;
pub mod prom_query;
mod sage;
pub mod spaceapi;
pub mod template_filters;
pub mod tools;
pub mod web;
pub mod wolfram;
