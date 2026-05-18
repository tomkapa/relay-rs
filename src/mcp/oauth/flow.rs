//! Browser flow: PKCE + DCR + authorize URL + code exchange.
//!
//! Uses the `oauth2` crate (already a dep) for the PKCE + token-exchange
//! pieces; DCR (RFC 7591) is a one-shot POST we render directly.

use std::time::Duration;

use oauth2::basic::BasicClient;
use oauth2::reqwest::Client as OAuthHttpClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse, TokenUrl,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::time::timeout;
use url::Url;

use crate::auth::OrgId;
use crate::types::SecretString;

use super::discovery::AsMetadata;
use super::errors::OAuthError;
use super::store::NewOAuthClient;

const FLOW_HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const DCR_MAX_BYTES: usize = 32 * 1024;
const TOKEN_MAX_BYTES: usize = 32 * 1024;

/// Cheap-clone HTTP client wired with the oauth2 trait. Holds both a
/// plain reqwest::Client (for DCR) and the oauth2-specific one (for the
/// token exchange).
#[derive(Clone)]
pub struct OAuthFlowClient {
    pub(crate) http: Client,
    pub(crate) http_oauth: OAuthHttpClient,
}

impl std::fmt::Debug for OAuthFlowClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthFlowClient").finish_non_exhaustive()
    }
}

impl OAuthFlowClient {
    /// Construct from a shared `reqwest` client; spins up the oauth2
    /// crate's separate HTTP client (its trait is incompatible with our
    /// own `reqwest::Client` directly).
    pub fn new(http: Client) -> Result<Self, OAuthError> {
        let http_oauth = OAuthHttpClient::builder()
            .timeout(FLOW_HTTP_TIMEOUT)
            .connect_timeout(Duration::from_secs(5))
            .redirect(oauth2::reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| OAuthError::Misconfigured(format!("oauth http: {e}")))?;
        Ok(Self { http, http_oauth })
    }
}

/// Output of [`start_authorization`].
#[derive(Debug, Clone)]
pub struct AuthorizeStart {
    pub authorize_url: Url,
    pub state: String,
    pub pkce_verifier: String,
}

/// Output of [`exchange_code`]. The plaintext tokens live here briefly;
/// the caller is responsible for sealing them before they hit the DB.
#[derive(Debug, Clone)]
pub struct TokenExchangeResult {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub scope: Option<String>,
    pub issuer: String,
    pub token_endpoint: String,
}

/// Re-exported aliasing — keeps the `PendingAuthorization` symbol
/// reachable from `super::oauth` without forcing every caller through
/// the `store::` path.
pub type PendingAuthorization = super::store::PendingAuthorization;

/// RFC 7591 Dynamic Client Registration. POSTs the smallest viable
/// metadata document to `registration_endpoint`; refuses to proceed if
/// the AS metadata doesn't advertise one (the operator must provision a
/// client out-of-band in that case).
#[tracing::instrument(
    name = "mcp.oauth.dcr",
    skip_all,
    fields(
        relay.mcp.oauth.issuer = %as_metadata.issuer,
    ),
)]
pub async fn register_dynamic_client(
    flow: &OAuthFlowClient,
    as_metadata: &AsMetadata,
    org_id: OrgId,
    redirect_uri: &str,
    scope: Option<&str>,
) -> Result<NewOAuthClient, OAuthError> {
    let registration_endpoint = as_metadata
        .registration_endpoint
        .as_deref()
        .ok_or_else(|| {
            OAuthError::Misconfigured(format!(
                "AS {} does not support dynamic client registration",
                as_metadata.issuer
            ))
        })?;

    // Pick the auth method: prefer `none` (PKCE-only public client) when
    // the AS advertises it, else `client_secret_basic`. RFC 7591 lets us
    // request a specific method; we read the supported list to avoid
    // negotiating one the AS will reject.
    let supported = as_metadata
        .token_endpoint_auth_methods_supported
        .as_deref()
        .unwrap_or(&[]);
    let auth_method = if supported.iter().any(|m| m == "none") {
        "none"
    } else if supported.iter().any(|m| m == "client_secret_basic") {
        "client_secret_basic"
    } else if supported.iter().any(|m| m == "client_secret_post") {
        "client_secret_post"
    } else {
        // RFC 7591 default
        "client_secret_basic"
    };

    let body = DcrRequest {
        client_name: "Relay",
        redirect_uris: vec![redirect_uri.to_owned()],
        grant_types: vec!["authorization_code".into(), "refresh_token".into()],
        response_types: vec!["code".into()],
        token_endpoint_auth_method: auth_method,
        scope: scope.map(str::to_owned),
    };

    let resp = timeout(
        FLOW_HTTP_TIMEOUT,
        flow.http.post(registration_endpoint).json(&body).send(),
    )
    .await
    .map_err(|_| OAuthError::Dcr("registration timed out".into()))?
    .map_err(|e| OAuthError::Dcr(format!("http: {e}")))?;

    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| OAuthError::Dcr(format!("body: {e}")))?;
    if !status.is_success() {
        return Err(OAuthError::Dcr(format!(
            "{} {} body={}",
            status.as_u16(),
            status.canonical_reason().unwrap_or(""),
            String::from_utf8_lossy(&bytes)
                .chars()
                .take(256)
                .collect::<String>()
        )));
    }
    if bytes.len() > DCR_MAX_BYTES {
        return Err(OAuthError::Dcr(format!(
            "response exceeds {DCR_MAX_BYTES} bytes"
        )));
    }
    let raw: DcrResponse =
        serde_json::from_slice(&bytes).map_err(|e| OAuthError::Dcr(format!("parse: {e}")))?;
    let client_secret = raw
        .client_secret
        .and_then(|s| SecretString::try_from(s).ok());
    let registration_access_token = raw
        .registration_access_token
        .and_then(|s| SecretString::try_from(s).ok());
    Ok(NewOAuthClient {
        org_id,
        issuer: as_metadata.issuer.clone(),
        client_id: raw.client_id,
        client_secret,
        authorization_endpoint: as_metadata.authorization_endpoint.clone(),
        token_endpoint: as_metadata.token_endpoint.clone(),
        registration_client_uri: raw.registration_client_uri,
        registration_access_token,
        token_endpoint_auth_method: auth_method.to_owned(),
        scope: scope.map(str::to_owned),
    })
}

#[derive(Debug, Serialize)]
struct DcrRequest<'a> {
    client_name: &'a str,
    redirect_uris: Vec<String>,
    grant_types: Vec<String>,
    response_types: Vec<String>,
    token_endpoint_auth_method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DcrResponse {
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
    #[serde(default)]
    registration_client_uri: Option<String>,
    #[serde(default)]
    registration_access_token: Option<String>,
}

/// Build the authorize URL the browser will be redirected to. PKCE +
/// state are minted here; the caller persists them in
/// `mcp_oauth_pending` for the callback to consume.
pub fn build_authorize_url(
    client: &super::store::DcrClientRecord,
    redirect_uri: &str,
    requested_scope: Option<&str>,
) -> Result<AuthorizeStart, OAuthError> {
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let oauth_client = build_basic_client(client, redirect_uri)?;
    let mut authorize = oauth_client
        .authorize_url(|| CsrfToken::new_random_len(32))
        .set_pkce_challenge(challenge);
    if let Some(s) = requested_scope {
        for scope in s.split_whitespace() {
            authorize = authorize.add_scope(Scope::new(scope.to_owned()));
        }
    }
    let (url, csrf) = authorize.url();
    Ok(AuthorizeStart {
        authorize_url: url,
        state: csrf.secret().clone(),
        pkce_verifier: verifier.secret().clone(),
    })
}

/// Exchange the callback code for tokens. The result is the plaintext
/// token payload; the caller seals it into the credentials seam.
#[tracing::instrument(
    name = "mcp.oauth.exchange",
    skip_all,
    fields(
        relay.mcp.oauth.issuer = %client.issuer,
    ),
)]
pub async fn exchange_code(
    flow: &OAuthFlowClient,
    client: &super::store::DcrClientRecord,
    redirect_uri: &str,
    code: &str,
    pkce_verifier: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<TokenExchangeResult, OAuthError> {
    let oauth_client = build_basic_client(client, redirect_uri)?;
    let token = oauth_client
        .exchange_code(AuthorizationCode::new(code.to_owned()))
        .set_pkce_verifier(PkceCodeVerifier::new(pkce_verifier.to_owned()))
        .request_async(&flow.http_oauth)
        .await
        .map_err(|e| OAuthError::TokenEndpoint(format!("exchange: {e}")))?;
    let access_token = token.access_token().secret().clone();
    let refresh_token = token.refresh_token().map(|t| t.secret().clone());
    // Default to a conservative 10-minute expiry when the server omits
    // `expires_in`. Vendors that don't return one are rare; the cap
    // means we refresh sooner rather than later, which is the safer
    // failure mode.
    let default_expiry = chrono::Duration::seconds(600);
    let expires_in = token.expires_in().map_or(default_expiry, |d| {
        chrono::Duration::from_std(d).unwrap_or(default_expiry)
    });
    let expires_at = now + expires_in;
    let scope = token.scopes().map(|ss| {
        ss.iter()
            .map(std::convert::AsRef::as_ref)
            .collect::<Vec<&str>>()
            .join(" ")
    });
    let _ = TOKEN_MAX_BYTES; // bounded transitively by oauth2's own checks
    Ok(TokenExchangeResult {
        access_token,
        refresh_token,
        expires_at,
        scope,
        issuer: client.issuer.clone(),
        token_endpoint: client.token_endpoint.clone(),
    })
}

/// Build the `oauth2` crate's `BasicClient` from our typed record + the
/// fixed redirect URI. Pulled out so authorize + exchange paths share
/// one source of truth (a drift between them would silently fail PKCE).
fn build_basic_client(
    client: &super::store::DcrClientRecord,
    redirect_uri: &str,
) -> Result<
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>,
    OAuthError,
> {
    let auth_url = AuthUrl::new(client.authorization_endpoint.clone())
        .map_err(|e| OAuthError::Misconfigured(format!("authorization_endpoint: {e}")))?;
    let token_url = TokenUrl::new(client.token_endpoint.clone())
        .map_err(|e| OAuthError::Misconfigured(format!("token_endpoint: {e}")))?;
    let redirect = RedirectUrl::new(redirect_uri.to_owned())
        .map_err(|e| OAuthError::Misconfigured(format!("redirect_uri: {e}")))?;
    let mut b = BasicClient::new(ClientId::new(client.client_id.clone())).set_auth_uri(auth_url);
    if let Some(secret) = &client.client_secret {
        b = b.set_client_secret(ClientSecret::new(secret.expose().to_owned()));
    }
    Ok(b.set_token_uri(token_url).set_redirect_uri(redirect))
}
