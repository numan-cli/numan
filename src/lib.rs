//! Numan library crate.
//!
//! Application handlers (`cmd`, `main`) return [`anyhow::Result`]. Library
//! modules exposed through this crate (for example [`nu::version_manager`])
//! return concrete [`thiserror::Error`] types so callers can match failures.

pub mod cli;
pub mod cmd;
pub mod config;
pub mod core;
pub mod install;
pub mod nu;
pub mod nupm_compat;
pub mod state;
pub mod util;
