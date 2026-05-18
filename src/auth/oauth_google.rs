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
use super::locale_hint::LocaleHint;
use super::types::{Email, GoogleProfile, GoogleSubject, OAuthState, PkceVerifier};

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
            .map_err(|e| AuthError::Misconfigured(format!("auth url: {e}")))?;
        let token = TokenUrl::new(GOOGLE_TOKEN_URL.to_owned())
            .map_err(|e| AuthError::Misconfigured(format!("token url: {e}")))?;
        let redirect = RedirectUrl::new(redirect_url.to_owned())
            .map_err(|e| AuthError::Misconfigured(format!("redirect url: {e}")))?;
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
            .map_err(|e| AuthError::Misconfigured(format!("userinfo url: {e}")))?;
        Ok(Self {
            client,
            http,
            userinfo_url,
        })
    }

    /// Build a redirect URL for the browser plus the (state, verifier)
    /// pair the caller must store for the upcoming callback. Both
    /// secrets pass through their newtype smart constructors so a
    /// future provider that emits malformed values fails fast here.
    pub fn start(&self) -> Result<AuthStart, AuthError> {
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, csrf) = self
            .client
            // 32 random bytes → 43-char base64url state, comfortably inside
            // the DB CHECK constraint (oauth_login_states.state octet_length
            // BETWEEN 32 AND 128) and the OAuthState newtype bounds. The
            // crate's `new_random` default emits only 16 bytes / 22 chars.
            .authorize_url(|| CsrfToken::new_random_len(32))
            .add_scope(Scope::new("openid".to_owned()))
            .add_scope(Scope::new("email".to_owned()))
            .add_scope(Scope::new("profile".to_owned()))
            .set_pkce_challenge(challenge)
            .url();
        Ok(AuthStart {
            authorize_url: url,
            state: OAuthState::try_from(csrf.secret().as_str())?,
            pkce_verifier: PkceVerifier::try_from(verifier.secret().as_str())?,
        })
    }

    /// Exchange the `code` from the callback for a userinfo profile. The
    /// HTTP client and timeouts wrap every external await per
    /// CLAUDE.md §5.
    pub async fn exchange(
        &self,
        code: &str,
        verifier: &PkceVerifier,
    ) -> Result<GoogleProfile, AuthError> {
        let http = self.http.clone();
        let token = self
            .client
            .exchange_code(AuthorizationCode::new(code.to_owned()))
            .set_pkce_verifier(PkceCodeVerifier::new(verifier.as_str().to_owned()))
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
            // PII (the email itself) is debug-tier and stripped by
            // production exporters per CLAUDE.md §2. The WARN line only
            // records the event so operators can monitor the rate of
            // unverified attempts without leaking subjects.
            warn!(event = "oauth.email_unverified");
            return Err(AuthError::EmailUnverified);
        }
        let email_raw = raw
            .email
            .ok_or_else(|| AuthError::OAuthProvider("userinfo missing email".into()))?;
        let email = Email::try_from(email_raw.as_str())?;
        let subject = GoogleSubject::try_from(raw.sub.as_str())?;
        // Google's `locale` is a hint; if it parses past the bound it's
        // dropped rather than failing the whole sign-in (a misshapen
        // hint just means we fall back to `Accept-Language` / DEFAULT).
        let locale = raw.locale.and_then(|raw| LocaleHint::try_from(raw).ok());
        Ok(GoogleProfile {
            subject,
            email,
            email_verified: true,
            display_name: raw.name,
            avatar_url: raw.picture,
            locale,
        })
    }
}

/// Output of [`GoogleOAuth::start`].
#[derive(Debug, Clone)]
pub struct AuthStart {
    pub authorize_url: Url,
    pub state: OAuthState,
    pub pkce_verifier: PkceVerifier,
}

/// Trait seam for tests. Production code uses [`GoogleOAuth`] directly.
#[async_trait]
pub trait TokenExchanger: Send + Sync + 'static {
    async fn exchange(
        &self,
        code: &str,
        verifier: &PkceVerifier,
    ) -> Result<GoogleProfile, AuthError>;
}

#[async_trait]
impl TokenExchanger for GoogleOAuth {
    async fn exchange(
        &self,
        code: &str,
        verifier: &PkceVerifier,
    ) -> Result<GoogleProfile, AuthError> {
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
    /// BCP-47 locale tag (e.g. `"vi"`, `"en-US"`). Optional — Google
    /// returns it for most accounts but not all. Propagated raw into
    /// `GoogleProfile.locale` and normalized later via
    /// `Language::from_locale_hint`.
    #[serde(default)]
    locale: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SecretString;

    fn dummy() -> GoogleOAuth {
        let cid = SecretString::try_from("test-client-id".to_owned()).expect("non-empty");
        let csec = SecretString::try_from("test-client-secret".to_owned()).expect("non-empty");
        GoogleOAuth::new(&cid, &csec, "http://localhost:8080/auth/google/callback")
            .expect("dummy client builds")
    }

    // Regression: the oauth2 crate's `CsrfToken::new_random` emits 16
    // random bytes → 22 base64url chars, which violates both the
    // `OAuthState::MIN_BYTES` newtype bound and the
    // `oauth_login_states.state` DB CHECK (octet_length BETWEEN 32 AND 128).
    // `start()` must produce a state that satisfies both.
    #[test]
    fn start_state_satisfies_newtype_and_db_check() {
        let oauth = dummy();
        let start = oauth.start().expect("start yields a valid AuthStart");
        let len = start.state.as_str().len();
        assert!(
            len >= OAuthState::MIN_BYTES,
            "state len {len} < newtype MIN {}",
            OAuthState::MIN_BYTES
        );
        assert!(
            len <= OAuthState::MAX_BYTES,
            "state len {len} > newtype MAX {}",
            OAuthState::MAX_BYTES
        );
        assert!(len >= 32, "state len {len} < DB CHECK min 32");
        assert!(len <= 128, "state len {len} > DB CHECK max 128");
    }

    #[test]
    fn start_pkce_verifier_satisfies_db_check() {
        let oauth = dummy();
        let start = oauth.start().expect("start yields a valid AuthStart");
        let len = start.pkce_verifier.as_str().len();
        assert!(len >= 32, "pkce_verifier len {len} < DB CHECK min 32");
        assert!(len <= 128, "pkce_verifier len {len} > DB CHECK max 128");
    }
}
