//! Fetch and model the public status of the platforms you depend on.
//!
//! Fetching and parsing (`providers`, `fetch`) stay apart from rendering (`render`, `serve`, `tui`)
//! so a future web UI can reuse everything but the output layer.

pub mod alerts;
pub mod config;
pub mod fetch;
pub mod history;
pub mod html;
pub mod model;
pub mod providers;
pub mod render;
pub mod serve;
pub mod tui;
