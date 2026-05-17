//! Per CLAUDE.md §5: every container/bound named here. No magic numbers
//! buried in business logic.

use std::time::Duration;

/// Cookie name carrying the session JWT.
pub const COOKIE_NAME: &str = "relay_session";

/// JWT validity window.
///
/// Hard expiry, no sliding refresh in v1 — user re-logs in once a week.
/// Picked to outlast a long working day in any timezone, but short
/// enough that a leaked token doesn't grant indefinite access.
pub const JWT_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// `oauth_login_states` row TTL. The Google flow happens within seconds;
/// 10 minutes is generous coverage for users who click "Sign in", make
/// coffee, then complete consent.
pub const OAUTH_STATE_TTL: Duration = Duration::from_secs(10 * 60);

/// Maximum slug-collision retries when minting the personal org.
///
/// Each retry appends a 4-char random suffix; after 5 attempts we fail
/// loudly — a clash that deep is a sign of a pathological email or a
/// corrupted PRNG.
pub const MAX_SLUG_RETRIES: usize = 5;

/// Maximum bytes accepted from Google's userinfo response. The spec
/// payload is well under 4 KiB; an oversize response is a sign of a
/// hijacked provider and is rejected at the boundary.
pub const MAX_USERINFO_BYTES: usize = 8 * 1024;

/// Hard timeout on every outbound call to Google during the OAuth
/// exchange. Wraps both the token exchange and the userinfo fetch.
pub const OAUTH_HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Minimum byte length of the JWT signing secret. HS256 best-practice
/// is 256 bits (32 bytes) of random material.
pub const JWT_SECRET_MIN_BYTES: usize = 32;
