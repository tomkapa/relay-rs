use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;

use relay_rs::{Settings, app, observability};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    observability::init();

    let settings = Settings::load().context("load settings")?;
    let server = app::build_server(settings).context("compose server")?;

    let cancel = CancellationToken::new();
    let watch = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("ctrl-c received; shutting down");
            watch.cancel();
        }
    });

    app::run_server(server, cancel).await.context("run server")
}
