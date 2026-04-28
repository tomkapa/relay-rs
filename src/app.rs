use std::sync::Arc;
use std::time::Duration;

use claudius::Anthropic;
use reqwest::Client;

use crate::config::Settings;
use crate::error::AppError;

const HTTP_USER_AGENT: &str = concat!("relay-rs/", env!("CARGO_PKG_VERSION"));
const HTTP_DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    anthropic: Anthropic,
    http: Client,
    settings: Settings,
}

impl AppState {
    pub fn from_settings(settings: Settings) -> Result<Self, AppError> {
        let mut anthropic = Anthropic::new(Some(settings.anthropic_api_key.clone()))?;
        if let Some(base_url) = settings.anthropic_base_url.clone() {
            anthropic = anthropic.with_base_url(base_url);
        }
        let http = Client::builder()
            .timeout(HTTP_DEFAULT_TIMEOUT)
            .user_agent(HTTP_USER_AGENT)
            .build()?;
        Ok(Self {
            inner: Arc::new(Inner {
                anthropic,
                http,
                settings,
            }),
        })
    }

    pub fn anthropic(&self) -> &Anthropic {
        &self.inner.anthropic
    }

    pub fn http(&self) -> &Client {
        &self.inner.http
    }

    pub fn settings(&self) -> &Settings {
        &self.inner.settings
    }
}
