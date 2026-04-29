mod cli;

use anyhow::{Context, Result};

use relay_rs::{Settings, app, observability};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    observability::init();

    let settings = Settings::load().context("load settings")?;
    let agent = app::build_agent(settings).context("compose agent")?;

    cli::run(agent).await
}
