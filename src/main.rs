mod cli;

use anyhow::{Context, Result};

use relay_rs::tools::default_registry;
use relay_rs::{Agent, AppState, MemoryManager, Settings, observability};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    observability::init();

    let settings = Settings::load().context("load settings")?;
    let state = AppState::from_settings(settings).context("init app state")?;

    let memory = MemoryManager::with_tools(default_registry(&state));
    let agent = Agent::new(state, memory);

    cli::run(&agent).await
}
