//! Google OAuth 2.0 Authorization Code + PKCE wrapper.
//!
//! Wraps the `oauth2` crate so that everything the rest of `auth/` sees
//! is a small, testable surface.
//!
//! `start()` mints a redirect URL + state; `exchange()` swaps a callback
//! `code` for a [`GoogleProfile`]. The token-exchange seam is behind
//! the [`TokenExchanger`] trait so tests can supply a deterministic
//! fake without driving a real OAuth client.

use std::time::Duration;

use async_trait::async_trait;
use oauth2::basic::BasicClient;
use oauth2::reqwest::Client as OAuthHttpClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse, TokenUrl,
};
use serde::Deserialize;
use tracing::warn;
use url::Url;

use crate::types::SecretString;

use super::error::AuthError;
use super::limits::{MAX_USERINFO_BYTES, OAUTH_HTTP_TIMEOUT};
use super::types::{Email, GoogleProfile};

const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_USERINFO_URL: &str = "https://openidconnect.googleapis.com/v1/userinfo";

/// Configured Google OAuth client. Cheap to clone.
#[derive(Clone)]
pub struct GoogleOAuth {
    client: ConfiguredClient,
    http: OAuthHttpClient,
    userinfo_url: Url,
}

type ConfiguredClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

impl std::fmt::Debug for GoogleOAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleOAuth").finish_non_exhaustive()
    }
}

impl GoogleOAuth {
    pub fn new(
        client_id: &SecretString,
        client_secret: &SecretString,
        redirect_url: &str,
    ) -> Result<Self, AuthError> {
        let auth = AuthUrl::new(GOOGLE_AUTH_URL.to_owned())
            .map_err(|e| AuthError::Internal_owned_string(format!("auth url: {e}")))?;
        let token = TokenUrl::new(GOOGLE_TOKEN_URL.to_owned())
            .map_err(|e| AuthError::Internal_owned_string(format!("token url: {e}")))?;
        let redirect = RedirectUrl::new(redirect_url.to_owned())
            .map_err(|e| AuthError::Internal_owned_string(format!("redirect url: {e}")))?;
        let client = BasicClient::new(ClientId::new(client_id.expose().to_owned()))
            .set_client_secret(ClientSecret::new(client_secret.expose().to_owned()))
            .set_auth_uri(auth)
            .set_token_uri(token)
            .set_redirect_uri(redirect);
        // oauth2 v5 ships its own reqwest re-export that implements the
        // crate's `AsyncHttpClient` trait. We use it for both the token
        // exchange *and* the userinfo fetch so the OAuth subsystem has
        // exactly one TLS stack to worry about, regardless of the project's
        // own (newer) reqwest version pinned elsewhere.
        let http = OAuthHttpClient::builder()
            .timeout(OAUTH_HTTP_TIMEOUT)
            .connect_timeout(Duration::from_secs(5))
            // Per Google OAuth docs the redirect must not be auto-followed
            // for token exchange. Keep the default redirect policy (none)
            // for safety; the userinfo endpoint does not redirect either.
            .redirect(oauth2::reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| AuthError::OAuthProvider(format!("http client: {e}")))?;
        let userinfo_url = Url::parse(GOOGLE_USERINFO_URL)
            .map_err(|e| AuthError::Internal_owned_string(format!("userinfo url: {e}")))?;
        Ok(Self {
            client,
            http,
            userinfo_url,
        })
    }

    /// Build a redirect URL for the browser plus the (state, verifier)
    /// pair the caller must store for the upcoming callback.
    #[must_use]
    pub fn start(&self) -> AuthStart {
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, csrf) = self
            .client
            .authorize_url(CsrfToken::new_random)
            .add_scope(Scope::new("openid".to_owned()))
            .add_scope(Scope::new("email".to_owned()))
            .add_scope(Scope::new("profile".to_owned()))
            .set_pkce_challenge(challenge)
            .url();
        AuthStart {
            authorize_url: url,
            state: csrf.secret().to_owned(),
            pkce_verifier: verifier.secret().to_owned(),
        }
    }

    /// Exchange the `code` from the callback for a userinfo profile. The
    /// HTTP client and timeouts wrap every external await per
    /// CLAUDE.md §5.
    pub async fn exchange(&self, code: &str, verifier: &str) -> Result<GoogleProfile, AuthError> {
        let http = self.http.clone();
        let token = self
            .client
            .exchange_code(AuthorizationCode::new(code.to_owned()))
            .set_pkce_verifier(PkceCodeVerifier::new(verifier.to_owned()))
            .request_async(&http)
            .await
            .map_err(|e| AuthError::OAuthProvider(format!("token exchange: {e}")))?;
        let access_token = token.access_token().secret();
        self.fetch_userinfo(access_token).await
    }

    async fn fetch_userinfo(&self, access_token: &str) -> Result<GoogleProfile, AuthError> {
        let resp = self
            .http
            .get(self.userinfo_url.clone())
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| AuthError::OAuthProvider(format!("userinfo: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(AuthError::OAuthProvider(format!("userinfo http {status}")));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AuthError::OAuthProvider(format!("userinfo body: {e}")))?;
        if bytes.len() > MAX_USERINFO_BYTES {
            return Err(AuthError::OAuthProvider(
                "userinfo response too large".into(),
            ));
        }
        let raw: GoogleUserinfo = serde_json::from_slice(&bytes)
            .map_err(|e| AuthError::OAuthProvider(format!("userinfo parse: {e}")))?;
        if !raw.email_verified.unwrap_or(false) {
            warn!(
                email = raw.email.as_deref().unwrap_or("?"),
                "oauth.email_unverified"
            );
            return Err(AuthError::EmailUnverified);
        }
        let email_raw = raw
            .email
            .ok_or_else(|| AuthError::OAuthProvider("userinfo missing email".into()))?;
        let email = Email::try_from(email_raw.as_str())?;
        Ok(GoogleProfile {
            subject: raw.sub,
            email,
            email_verified: true,
            display_name: raw.name,
            avatar_url: raw.picture,
        })
    }
}

/// Output of [`GoogleOAuth::start`].
#[derive(Debug, Clone)]
pub struct AuthStart {
    pub authorize_url: Url,
    pub state: String,
    pub pkce_verifier: String,
}

/// Trait seam for tests. Production code uses [`GoogleOAuth`] directly.
#[async_trait]
pub trait TokenExchanger: Send + Sync + 'static {
    async fn exchange(&self, code: &str, verifier: &str) -> Result<GoogleProfile, AuthError>;
}

#[async_trait]
impl TokenExchanger for GoogleOAuth {
    async fn exchange(&self, code: &str, verifier: &str) -> Result<GoogleProfile, AuthError> {
        Self::exchange(self, code, verifier).await
    }
}

#[derive(Debug, Deserialize)]
struct GoogleUserinfo {
    sub: String,
    email: Option<String>,
    email_verified: Option<bool>,
    name: Option<String>,
    picture: Option<String>,
}

// Helper to keep AuthError::Internal taking &'static str ergonomic when
// the message is dynamic — we box-leak via Box::leak would create a
// memory leak; instead we extend the AuthError surface here.
impl AuthError {
    #[allow(non_snake_case)]
    fn Internal_owned_string(msg: String) -> Self {
        // Lean on OAuthProvider for dynamic strings during startup
        // parsing — these would otherwise need a boxed String variant.
        // OAuthProvider semantically also covers misconfigured endpoint
        // URLs (which are part of provider integration).
        Self::OAuthProvider(msg)
    }
}
