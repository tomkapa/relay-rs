use std::net::SocketAddr;
use std::str::FromStr;

use chrono_tz::Tz;
use config::{Config, ConfigError, Environment};
use serde::Deserialize;
use thiserror::Error;

use crate::types::{ModelId, SecretString};

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("config source: {0}")]
    Source(#[from] ConfigError),

    #[error("no provider api key set; set exactly one of: OPENAI_API_KEY, ANTHROPIC_API_KEY")]
    NoProviderKey,

    #[error(
        "multiple provider api keys set ({set:?}); set only one of: OPENAI_API_KEY, ANTHROPIC_API_KEY"
    )]
    MultipleProviderKeys { set: Vec<&'static str> },

    #[error("embedding configuration missing; set EMBEDDING_API_KEY and EMBEDDING_MODEL")]
    MissingEmbedding,

    #[error("default timezone {raw:?} is not a valid IANA name")]
    InvalidDefaultTimezone { raw: String },
}

/// Process-wide configuration loaded once at startup. Secrets are wrapped in
/// [`SecretString`] so a stray `tracing::debug!(?settings)` cannot leak them.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Selected LLM backend plus the credentials it needs. Parsed once at startup so
    /// `build_provider` is an infallible exhaustive match.
    pub provider: ProviderSettings,
    pub brave_search_api_key: SecretString,
    pub model: ModelId,
    pub http_addr: SocketAddr,
    /// Postgres connection string. Required at startup — there is no in-memory
    /// fallback. Wrapped in [`SecretString`] because the URL embeds a password.
    pub database_url: SecretString,
    /// Embedding provider configuration. Required: the memory subsystem
    /// refuses to start without one. Decoupled from the chat provider so
    /// chat and embeddings can point at different vendors.
    pub embedding: EmbeddingSettings,
    /// Process-wide fallback IANA timezone applied when an agent calls
    /// `schedule_task` without specifying `tz`. Future: per-organisation
    /// override loaded by id; until then the resolver hands every caller
    /// this same value.
    pub default_timezone: Tz,
}

/// Embedding-provider settings — `EMBEDDING_API_KEY` /
/// `EMBEDDING_BASE_URL` / `EMBEDDING_MODEL`. Required as a group:
/// either all three (api_key + model, base_url optional) or none.
#[derive(Debug, Clone)]
pub struct EmbeddingSettings {
    pub api_key: SecretString,
    pub base_url: Option<String>,
    pub model: String,
    /// Vector dimension produced by the model. Must match the
    /// `agent_memories.embedding` column (1536 in migration 9).
    pub dimensions: usize,
}

/// Provider selection + the credentials that go with it. Exhaustive — adding a backend
/// means a new variant here and a new arm in `app::build_provider`.
#[derive(Debug, Clone)]
pub enum ProviderSettings {
    Openai {
        api_key: SecretString,
        base_url: Option<String>,
    },
    Anthropic {
        api_key: SecretString,
        base_url: Option<String>,
    },
}

impl ProviderSettings {
    /// Low-cardinality identifier for tracing fields (`relay.provider.selected`).
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Openai { .. } => "openai",
            Self::Anthropic { .. } => "anthropic",
        }
    }
}

/// Flat env shape — every provider's credentials are optional. The presence of exactly
/// one provider's `*_API_KEY` selects which provider runs; setting zero or more than one
/// is a misconfiguration and rejected at the boundary. Kept private because `Settings`
/// is the validated type.
#[derive(Debug, Deserialize)]
struct RawSettings {
    #[serde(default)]
    openai_api_key: Option<SecretString>,
    #[serde(default)]
    openai_base_url: Option<String>,

    #[serde(default)]
    anthropic_api_key: Option<SecretString>,
    #[serde(default)]
    anthropic_base_url: Option<String>,

    brave_search_api_key: SecretString,
    #[serde(default = "default_model")]
    model: ModelId,
    #[serde(default = "default_http_addr")]
    http_addr: SocketAddr,
    database_url: SecretString,

    #[serde(default)]
    embedding_api_key: Option<SecretString>,
    #[serde(default)]
    embedding_base_url: Option<String>,
    #[serde(default)]
    embedding_model: Option<String>,
    #[serde(default)]
    embedding_dimensions: Option<usize>,

    #[serde(default = "default_timezone_raw")]
    default_timezone: String,
}

fn default_timezone_raw() -> String {
    "UTC".to_string()
}

fn default_model() -> ModelId {
    ModelId::try_from("claude-sonnet-4-5").expect("static default model id is valid")
}

fn default_http_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 8080))
}

impl TryFrom<RawSettings> for Settings {
    type Error = SettingsError;

    fn try_from(raw: RawSettings) -> Result<Self, Self::Error> {
        // Provider is inferred from which `*_API_KEY` is set. Refusing the ambiguous
        // "both set" case is intentional: silently picking one would mask a copy-paste
        // bug in the operator's environment for the cost of a clearer error here.
        let provider = match (raw.openai_api_key, raw.anthropic_api_key) {
            (Some(api_key), None) => ProviderSettings::Openai {
                api_key,
                base_url: raw.openai_base_url,
            },
            (None, Some(api_key)) => ProviderSettings::Anthropic {
                api_key,
                base_url: raw.anthropic_base_url,
            },
            (None, None) => return Err(SettingsError::NoProviderKey),
            (Some(_), Some(_)) => {
                return Err(SettingsError::MultipleProviderKeys {
                    set: vec!["OPENAI_API_KEY", "ANTHROPIC_API_KEY"],
                });
            }
        };
        let embedding = match (raw.embedding_api_key, raw.embedding_model) {
            (Some(api_key), Some(model)) => EmbeddingSettings {
                api_key,
                base_url: raw.embedding_base_url,
                model,
                dimensions: raw
                    .embedding_dimensions
                    .unwrap_or(DEFAULT_EMBEDDING_DIMENSIONS),
            },
            _ => return Err(SettingsError::MissingEmbedding),
        };
        let default_timezone = Tz::from_str(&raw.default_timezone).map_err(|_| {
            SettingsError::InvalidDefaultTimezone {
                raw: raw.default_timezone.clone(),
            }
        })?;
        Ok(Self {
            provider,
            brave_search_api_key: raw.brave_search_api_key,
            model: raw.model,
            http_addr: raw.http_addr,
            database_url: raw.database_url,
            embedding,
            default_timezone,
        })
    }
}

/// Default vector dimension. Matches `text-embedding-3-small` / the
/// `agent_memories.embedding` column committed in migration 9. Operators
/// pointing at a model with a different dimension must override
/// `EMBEDDING_DIMENSIONS` *and* run a custom column migration.
const DEFAULT_EMBEDDING_DIMENSIONS: usize = 1536;

impl Settings {
    /// Load settings from environment variables. Missing required values surface as a
    /// `SettingsError` so the caller can decide how to report.
    pub fn load() -> Result<Self, SettingsError> {
        let raw: RawSettings = Config::builder()
            .add_source(Environment::default())
            .build()?
            .try_deserialize()?;
        Self::try_from(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(s: &str) -> SecretString {
        SecretString::try_from(s.to_string()).expect("non-empty")
    }

    fn empty_raw() -> RawSettings {
        // Embedding settings are required per doc/memory.md §2.9 — fill
        // them in for the cases that expect a successful parse; tests that
        // probe the no-embedding error path overwrite them back to None.
        RawSettings {
            openai_api_key: None,
            openai_base_url: None,
            anthropic_api_key: None,
            anthropic_base_url: None,
            brave_search_api_key: secret("brave"),
            model: default_model(),
            http_addr: default_http_addr(),
            database_url: secret("postgres://relay:relay@localhost:5432/relay"),
            embedding_api_key: Some(secret("emb")),
            embedding_base_url: None,
            embedding_model: Some("text-embedding-3-small".to_string()),
            embedding_dimensions: None,
            default_timezone: default_timezone_raw(),
        }
    }

    #[test]
    fn no_provider_key_is_rejected() {
        let raw = empty_raw();
        let err = Settings::try_from(raw).expect_err("expected error");
        assert!(matches!(err, SettingsError::NoProviderKey));
    }

    #[test]
    fn both_provider_keys_set_is_rejected() {
        let mut raw = empty_raw();
        raw.openai_api_key = Some(secret("sk-x"));
        raw.anthropic_api_key = Some(secret("sk-ant"));
        let err = Settings::try_from(raw).expect_err("expected error");
        assert!(matches!(err, SettingsError::MultipleProviderKeys { .. }));
    }

    #[test]
    fn openai_key_alone_selects_openai() {
        let mut raw = empty_raw();
        raw.openai_api_key = Some(secret("sk-x"));
        raw.openai_base_url = Some("https://api.deepseek.com/v1".to_string());
        let s = Settings::try_from(raw).expect("valid");
        let ProviderSettings::Openai { base_url, .. } = &s.provider else {
            panic!("expected openai");
        };
        assert_eq!(base_url.as_deref(), Some("https://api.deepseek.com/v1"));
    }

    #[test]
    fn anthropic_key_alone_selects_anthropic() {
        let mut raw = empty_raw();
        raw.anthropic_api_key = Some(secret("sk-ant"));
        let s = Settings::try_from(raw).expect("valid");
        assert_eq!(s.provider.name(), "anthropic");
    }

    #[test]
    fn invalid_default_timezone_is_rejected() {
        let mut raw = empty_raw();
        raw.openai_api_key = Some(secret("sk-x"));
        raw.default_timezone = "Mars/Olympus_Mons".to_string();
        let err = Settings::try_from(raw).expect_err("expected error");
        assert!(matches!(err, SettingsError::InvalidDefaultTimezone { .. }));
    }

    #[test]
    fn default_timezone_defaults_to_utc() {
        let mut raw = empty_raw();
        raw.openai_api_key = Some(secret("sk-x"));
        let s = Settings::try_from(raw).expect("valid");
        assert_eq!(s.default_timezone, Tz::UTC);
    }

    #[test]
    fn default_timezone_parses_iana_name() {
        let mut raw = empty_raw();
        raw.openai_api_key = Some(secret("sk-x"));
        raw.default_timezone = "Asia/Bangkok".to_string();
        let s = Settings::try_from(raw).expect("valid");
        assert_eq!(s.default_timezone, chrono_tz::Asia::Bangkok);
    }

    #[test]
    fn missing_embedding_is_rejected() {
        let mut raw = empty_raw();
        raw.openai_api_key = Some(secret("sk-x"));
        raw.embedding_api_key = None;
        raw.embedding_model = None;
        let err = Settings::try_from(raw).expect_err("expected error");
        assert!(matches!(err, SettingsError::MissingEmbedding));
    }
}
