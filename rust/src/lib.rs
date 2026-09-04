//! Snap — a small local version control system with vector-clock versions,
//! patch-based history, and deterministic automatic merging.
//!
//! `SPEC.md` at the capstone root is the canonical behavioural contract; every
//! module here cites the section it implements. All logic lives in this library
//! so it is reachable from unit tests, integration tests, and benchmarks; the
//! `snap` binary is a thin shell over [`run`].

pub mod b64;
pub mod cli;
pub mod config;
pub mod error;
pub mod http;
pub mod json;
pub mod model;
pub mod ot;
pub mod present;
pub mod render;
pub mod replay;
pub mod text;
pub mod validate;
pub mod version;
pub mod worktree;

pub use error::{Error, Result};
