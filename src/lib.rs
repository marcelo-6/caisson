#![forbid(unsafe_code)]
#![doc = r#"
`caisson` is the backend for the offline updater.


Right now the crate is simple: it knows how to load a predefined
service catalog, accept a local `.edgepkg` (name might not be final yet), validate it conservatively,
import the staged image into Docker, and persist the result.
"#]

pub mod app;
pub mod audit;
pub mod cli;
pub mod compose;
pub mod config;
pub mod docker;
pub mod domain;
pub mod package;
pub mod persistence;
pub mod update;

/// Current updater version as reported by the crate metadata.
pub const UPDATER_VERSION: &str = env!("CARGO_PKG_VERSION");
