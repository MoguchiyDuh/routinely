pub mod app;
pub mod auth;

pub const ASSET_VERSION: &str = env!("ASSET_VERSION");
pub mod avatar;
pub mod config;
pub mod cookie;
pub mod db;
pub mod kv;
pub mod punish;
pub mod recurrence;
pub mod score;
pub mod store;
pub mod web;
