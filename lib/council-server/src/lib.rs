#![warn(
    clippy::unwrap_in_result,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::panic,
    clippy::missing_panics_doc,
    clippy::panic_in_result_fn,
    missing_docs
)]
#![allow(
    missing_docs, // TODO(fnichol): remove when ready to document crate
    clippy::missing_errors_doc,
    clippy::module_inception,
    clippy::module_name_repetitions
)]

mod app_state;
mod config;
mod kv;
mod server;
mod state_machine;

pub use self::{
    config::{Config, ConfigBuilder, ConfigError, ConfigFile, StandardConfig, StandardConfigFile},
    kv::{DependencyGraphData, DependencyGraphDataEntry, KvRevision, StateStore, StatusStore},
    server::{Error, Server},
    state_machine::StateMachine,
};
