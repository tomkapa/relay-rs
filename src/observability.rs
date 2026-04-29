//! Tracing pipeline.
//!
//! Today: a `tracing-subscriber` formatter writing to stderr. Tomorrow: bridge to OTLP
//! via `tracing-opentelemetry` so spans flow to a collector. Span / event names already
//! follow the `relay.*` semantic-convention prefix mandated by CLAUDE.md §2; wiring in
//! the OTel exporter is then a one-file change here.

use std::io;

use tracing_subscriber::EnvFilter;

const DEFAULT_FILTER: &str = "relay_rs=info,claudius=warn";

pub fn init() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(io::stderr)
        .init();
}
