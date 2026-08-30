//! Phase 0 core primitives.
//!
//! No Rime, llama.cpp, or platform IPC implementation belongs here yet. This crate owns
//! validation and revision handling so later service code has one source of truth.

pub mod config;
pub mod error;

pub use config::{validate, validate_with, ConfigStore, ConfigValidationError, Limits};
pub use error::CoreError;
pub use lime_protocol::{Config, ConfigSnapshot};
