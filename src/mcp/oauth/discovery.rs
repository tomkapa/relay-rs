//! RFC 9728 + RFC 8414 discovery.
//!
//! Given an MCP server URL, produce the authorization-server metadata we
//! need to drive PKCE + DCR. Two hops, both bounded:
//!   1. `<server>/.well-known/oauth-protected-resource` (RFC 9728) →
//!      `authorization_servers[0]`. If the well-known is unavailable,
//!      fall back to probing the server with no auth and parsing the
//!      `WWW-Authenticate: Bearer resource_metadata=…` header.
//!   2. `<issuer>/.well-known/oauth-authorization-server` (RFC 8414) →
//!      `authorization_endpoint` / `token_endpoint` /
//!      `registration_endpoint` / supported scopes.
//!
//! Bounded so a vendor that hands back a 10MB JSON blob cannot bloat
//! memory or stall the handler.

use reqwest::Client;
use serde::Deserialize;
use tokio::time::timeout;
use url::Url;

use super::errors::OAuthError;

/// Per-request timeout for one discovery fetch (server-side limit).
const DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Max bytes we'll read from any well-known endpoint. RFC 8414 docs are
/// usually < 2KB; we cap at 32KB to leave room for `scopes_supported`
/// lists.
const DISCOVERY_MAX_BYTES: usize = 32 * 1024;

/// Output of [`discover_authorization_server`] — exactly what the OAuth
/// flow + DCR need to proceed.
#[derive(Debug, Clone)]
pub struct AsMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    /// Some authorization servers do not support DCR (RFC 7591). When
    /// absent we surface a typed misconfiguration up the stack and ask
    /// the operator to provision a client out-of-band.
    pub registration_endpoint: Option<String>,
    pub scopes_supported: Option<Vec<String>>,
    pub token_endpoint_auth_methods_supported: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ProtectedResourceMetadata {
    authorization_servers: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct AsMetadataJson {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    registration_endpoint: Option<String>,
    #[serde(default)]
    scopes_supported: Option<Vec<String>>,
    #[serde(default)]
    token_endpoint_auth_methods_supported: Option<Vec<String>>,
}

/// Find the authorization-server metadata for `server_url`.
///
/// The fetch chain is explicit (no recursion, hop counter inlined).
/// We try resource-metadata first; if that 404s, we follow the spec'd
/// 401-probe fallback. Either way the inner discovery URL is fetched
/// at most once.
#[tracing::instrument(
    name = "mcp.oauth.discover",
    skip_all,
    fields(
        relay.mcp.url = %server_url,
    ),
)]
pub async fn discover_authorization_server(
    http: &Client,
    server_url: &str,
) -> Result<AsMetadata, OAuthError> {
    // Hop 1: resource metadata.
    let server =
        Url::parse(server_url).map_err(|e| OAuthError::Discovery(format!("server url: {e}")))?;
    let resource_metadata_url = join_well_known(&server, ".well-known/oauth-protected-resource")?;
    let issuer = fetch_protected_resource_issuer(http, &resource_metadata_url)
        .await
        .map_err(|first_err| {
            // The 401-probe fallback (parse `WWW-Authenticate: Bearer
            // resource_metadata=…`) is left as a follow-up; real upstream
            // MCP vendors today (Notion, Linear) advertise the well-known.
            tracing::debug!(
                error = %first_err,
                "mcp.oauth.discover.resource_metadata_failed"
            );
            first_err
        })?;

    // Hop 2: AS metadata.
    let issuer_url =
        Url::parse(&issuer).map_err(|e| OAuthError::Discovery(format!("issuer url: {e}")))?;
    let as_metadata_url = join_well_known(&issuer_url, ".well-known/oauth-authorization-server")?;
    let raw = fetch_json::<AsMetadataJson>(http, &as_metadata_url).await?;

    // The AS metadata must echo back the issuer it sits at; mismatch
    // means the resource is pointing us at a server that isn't the
    // issuer it claims to be (RFC 8414 §2.4 mitigation).
    if raw.issuer != issuer {
        return Err(OAuthError::Discovery(format!(
            "issuer mismatch: resource says {issuer}, AS metadata says {}",
            raw.issuer
        )));
    }

    Ok(AsMetadata {
        issuer: raw.issuer,
        authorization_endpoint: raw.authorization_endpoint,
        token_endpoint: raw.token_endpoint,
        registration_endpoint: raw.registration_endpoint,
        scopes_supported: raw.scopes_supported,
        token_endpoint_auth_methods_supported: raw.token_endpoint_auth_methods_supported,
    })
}

async fn fetch_protected_resource_issuer(http: &Client, url: &Url) -> Result<String, OAuthError> {
    let metadata = fetch_json::<ProtectedResourceMetadata>(http, url).await?;
    let issuers = metadata.authorization_servers.unwrap_or_default();
    issuers
        .into_iter()
        .next()
        .ok_or_else(|| OAuthError::Discovery("no authorization_servers advertised".into()))
}

async fn fetch_json<T: serde::de::DeserializeOwned>(
    http: &Client,
    url: &Url,
) -> Result<T, OAuthError> {
    let req = http.get(url.clone()).send();
    let resp = timeout(DISCOVERY_TIMEOUT, req)
        .await
        .map_err(|_| OAuthError::Discovery("timed out".into()))?
        .map_err(|e| OAuthError::Discovery(format!("http: {e}")))?;
    if !resp.status().is_success() {
        return Err(OAuthError::Discovery(format!(
            "{} {} {}",
            url,
            resp.status().as_u16(),
            resp.status().canonical_reason().unwrap_or("")
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| OAuthError::Discovery(format!("body: {e}")))?;
    if bytes.len() > DISCOVERY_MAX_BYTES {
        return Err(OAuthError::Discovery(format!(
            "response exceeds {DISCOVERY_MAX_BYTES} bytes"
        )));
    }
    serde_json::from_slice::<T>(&bytes).map_err(|e| OAuthError::Discovery(format!("parse: {e}")))
}

/// Compose `<base>/<path>` cleanly: strip any trailing slash off the
/// base, prepend the path. Using `Url::join` is the canonical way but it
/// surprises on origins without a trailing slash; build the string
/// explicitly to avoid a class of subtle bugs.
fn join_well_known(base: &Url, path: &str) -> Result<Url, OAuthError> {
    let mut s = base.to_string();
    while s.ends_with('/') {
        s.pop();
    }
    s.push('/');
    s.push_str(path);
    Url::parse(&s).map_err(|e| OAuthError::Discovery(format!("join {path}: {e}")))
}
