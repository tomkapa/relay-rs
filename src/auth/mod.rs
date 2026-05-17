//! Authentication + tenancy primitives.
//!
//! - Identity types ([`UserId`], [`OrgId`], [`Email`], [`Role`], [`Principal`]).
//! - JWT-in-cookie session ([`JwtSigner`]).
//! - Google OAuth wrapper ([`GoogleOAuth`]).
//! - Store contract over `users`, `organizations`, `org_members`,
//!   `user_identities`, `oauth_login_states` ([`UserStore`]).
//! - Tenant-context helpers ([`begin_as`], [`begin_privileged`]).

mod error;
mod jwt;
pub mod limits;
mod oauth_google;
mod pg_store;
mod store;
mod types;

pub use error::AuthError;
pub use jwt::{JwtClaims, JwtSigner};
pub use oauth_google::{AuthStart, GoogleOAuth, TokenExchanger};
pub use pg_store::PgUserStore;
pub use store::{
    ConsumedOAuthState, NewOrg, OAuthStateRow, SharedUserStore, UpsertedUser, UserStore,
};
pub use types::{
    Email, GoogleProfile, OrgId, OrgMembership, OrgSlug, Principal, Role, User, UserId,
};

use sqlx::{PgPool, Postgres, Transaction};

/// Open a transaction with `app.user_id` set to the principal's user id.
///
/// `SET LOCAL` (the `set_config(..., true)` SQL form) scopes the value
/// to this transaction so a connection returned to the pool without
/// commit/rollback cannot leak the value to the next checkout. RLS
/// policies on every domain table read `app_current_user_id()` and
/// check membership through `app_user_is_member(row.org_id)`, so
/// subsequent queries automatically filter to rows the principal can
/// see.
#[allow(clippy::elidable_lifetime_names)]
pub async fn begin_as<'a>(
    pool: &'a PgPool,
    principal: &Principal,
) -> Result<Transaction<'a, Postgres>, AuthError> {
    begin_as_user(pool, principal.user_id).await
}

/// Like [`begin_as`] but for worker-side code paths where the principal
/// is derived from a row (e.g. `sessions.created_by_user_id`) rather
/// than a request cookie.
#[allow(clippy::elidable_lifetime_names)]
pub async fn begin_as_user<'a>(
    pool: &'a PgPool,
    user_id: UserId,
) -> Result<Transaction<'a, Postgres>, AuthError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('app.user_id', $1, true)")
        .bind(user_id.as_uuid().to_string())
        .execute(&mut *tx)
        .await?;
    // Drop privileges to the non-super `relay_app` role so RLS policies
    // actually apply. Postgres superusers bypass RLS unconditionally —
    // even with FORCE ROW LEVEL SECURITY — so the table owner identity
    // most apps use in dev (here, `relay`) wouldn't be isolated without
    // this. RESET ROLE happens automatically on commit/rollback.
    sqlx::query("SET LOCAL ROLE relay_app")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

/// Open a transaction that bypasses RLS. Use only for infrastructure
/// code (queue claim, scheduler scans, migrations) — never for HTTP
/// request handling.
///
/// Works because the app role is the table owner in dev and tests;
/// production deployments that split roles should swap this for a
/// dedicated `BYPASSRLS` role.
///
/// Returns `sqlx::Error` (not `AuthError`) so callers in any subsystem
/// can use `?` with their existing `Db(#[from] sqlx::Error)` variant.
/// Routing every caller's error type through `AuthError` would require
/// `From<AuthError>` on `SessionError`, `MemoryStoreError`,
/// `ScheduledTaskError`, `PromptError`, `McpError`, `ResponseError`,
/// and `AgentStoreError` — a cross-module change disproportionate to
/// the benefit. This helper is infrastructure-level; the inner failure
/// is always a Postgres error.
pub async fn begin_privileged(pool: &PgPool) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL row_security = off")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}
