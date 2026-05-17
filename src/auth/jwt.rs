//! HS256 JWT signer for session cookies. Per CLAUDE.md §11 time enters
//! through [`SharedClock`]; never `Utc::now`.

use std::time::{Duration, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

use crate::clock::SharedClock;
use crate::types::SecretString;

use super::error::AuthError;
use super::limits::{JWT_SECRET_MIN_BYTES, JWT_TTL};
use super::types::{OrgId, UserId};

/// Claims minted on login and consumed by [`crate::http::auth_layer`].
/// `sub` = user id, `org` = active org id, `exp/iat` = epoch seconds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtClaims {
    pub sub: UserId,
    pub org: OrgId,
    pub iat: i64,
    pub exp: i64,
}

/// Process-wide signer. Cheap to clone — `EncodingKey` and `DecodingKey`
/// are reference-counted internally by `jsonwebtoken`.
#[derive(Clone)]
pub struct JwtSigner {
    encoding: EncodingKey,
    decoding: DecodingKey,
    ttl: Duration,
    clock: SharedClock,
}

impl std::fmt::Debug for JwtSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtSigner")
            .field("ttl_seconds", &self.ttl.as_secs())
            .finish_non_exhaustive()
    }
}

impl JwtSigner {
    /// Construct a signer from a shared secret. Fails fast if the
    /// secret is too short to be safe for HS256.
    pub fn new(secret: &SecretString, clock: SharedClock) -> Result<Self, AuthError> {
        let bytes = secret.expose().as_bytes();
        if bytes.len() < JWT_SECRET_MIN_BYTES {
            return Err(AuthError::Internal(
                "jwt secret too short (need >= 32 bytes)",
            ));
        }
        Ok(Self {
            encoding: EncodingKey::from_secret(bytes),
            decoding: DecodingKey::from_secret(bytes),
            ttl: JWT_TTL,
            clock,
        })
    }

    /// Override the default TTL — for tests that want short-lived tokens.
    #[must_use]
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Mint a JWT for `(user, org)` with the configured TTL.
    pub fn mint(&self, user: UserId, org: OrgId) -> Result<String, AuthError> {
        let now = self.now_epoch();
        let claims = JwtClaims {
            sub: user,
            org,
            iat: now,
            exp: now.saturating_add(i64::try_from(self.ttl.as_secs()).unwrap_or(i64::MAX)),
        };
        encode(&Header::new(Algorithm::HS256), &claims, &self.encoding)
            .map_err(|e| AuthError::Jwt(e.to_string()))
    }

    /// Verify signature + expiry; return the decoded claims.
    pub fn verify(&self, token: &str) -> Result<JwtClaims, AuthError> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.leeway = 5;
        validation.validate_exp = true;
        validation.required_spec_claims = ["exp", "iat"].iter().map(|s| (*s).to_owned()).collect();
        let data = decode::<JwtClaims>(token, &self.decoding, &validation)
            .map_err(|e| AuthError::Jwt(e.to_string()))?;
        Ok(data.claims)
    }

    fn now_epoch(&self) -> i64 {
        let secs = self
            .clock
            .now_wall()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        i64::try_from(secs).unwrap_or(i64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::clock::SystemClock;
    use crate::types::SecretString;

    use super::super::types::{OrgId, UserId};
    use super::JwtSigner;

    fn signer() -> JwtSigner {
        let secret = SecretString::try_from("a".repeat(64)).expect("non-empty");
        JwtSigner::new(&secret, SystemClock::shared()).expect("signer")
    }

    #[test]
    fn rejects_short_secret() {
        let secret = SecretString::try_from("short".to_owned()).expect("non-empty");
        assert!(JwtSigner::new(&secret, SystemClock::shared()).is_err());
    }

    #[test]
    fn round_trip() {
        let s = signer();
        let user = UserId::new();
        let org = OrgId::new();
        let token = s.mint(user, org).expect("mint");
        let claims = s.verify(&token).expect("verify");
        assert_eq!(claims.sub.as_uuid(), user.as_uuid());
        assert_eq!(claims.org.as_uuid(), org.as_uuid());
        assert!(claims.exp > claims.iat);
    }

    #[test]
    fn tampered_signature_rejected() {
        let s = signer();
        let token = s.mint(UserId::new(), OrgId::new()).expect("mint");
        let mut bytes: Vec<u8> = token.into_bytes();
        // Flip a bit deep in the signature.
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        let tampered = String::from_utf8(bytes).expect("valid utf8");
        assert!(s.verify(&tampered).is_err());
    }

    #[test]
    fn expired_token_rejected() {
        let s = signer().with_ttl(Duration::from_secs(1));
        let token = s.mint(UserId::new(), OrgId::new()).expect("mint");
        // Wait past the leeway window (Validation::leeway = 5s above).
        std::thread::sleep(Duration::from_secs(7));
        assert!(s.verify(&token).is_err());
    }
}
