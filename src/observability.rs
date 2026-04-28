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
