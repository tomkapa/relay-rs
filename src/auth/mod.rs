//! Authentication + tenancy primitives.
//!
//! - Identity types ([`UserId`], [`OrgId`], [`Email`], [`Role`], [`Principal`]).
//! - JWT-in-cookie session ([`JwtSigner`]).
//! - Google OAuth wrapper ([`GoogleOAuth`]).
//! - Store contract over `users`, `organizations`, `org_members`,
//!   `user_identities`, `oauth_login_states` ([`UserStore`]).
//! - Tenant-context helpers ([`begin_as`], [`begin_privileged`], [`TxScope`]).

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
    Email, GoogleProfile, GoogleSubject, OAuthState, OrgId, OrgMembership, OrgSlug, PkceVerifier,
    Principal, Role, User, UserId,
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
    begin_as_user(pool, principal.user_id)
        .await
        .map_err(AuthError::from)
}

/// Like [`begin_as`] but for worker-side code paths where the principal
/// is derived from a row (e.g. `sessions.created_by_user_id`) rather
/// than a request cookie.
///
/// Returns `sqlx::Error` (not `AuthError`) so every subsystem's error
/// enum can absorb it via its existing `Db(#[from] sqlx::Error)` variant.
/// Every inner failure here is a Postgres error.
#[allow(clippy::elidable_lifetime_names)]
pub async fn begin_as_user<'a>(
    pool: &'a PgPool,
    user_id: UserId,
) -> Result<Transaction<'a, Postgres>, sqlx::Error> {
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
pub async fn begin_privileged(pool: &PgPool) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL row_security = off")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

/// Tenant-context discriminator shared by every domain store.
///
/// `Privileged` opens a [`begin_privileged`] tx for cross-tenant
/// infrastructure paths (queue claim, scheduler scans); `AsUser` opens
/// a [`begin_as_user`] tx pinned to the acting principal so RLS WITH
/// CHECK fires on writes. Used by `memory`, `agents`, `session`,
/// `mcp`, `runtime`, and `scheduling` stores.
#[derive(Debug, Clone, Copy)]
pub enum TxScope {
    Privileged,
    AsUser(UserId),
}

impl TxScope {
    /// Open the appropriate transaction for this scope. Returns
    /// `sqlx::Error` so any module error enum carrying
    /// `Db(#[from] sqlx::Error)` absorbs failures via `?`.
    pub async fn begin(self, pool: &PgPool) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
        match self {
            Self::Privileged => begin_privileged(pool).await,
            Self::AsUser(user_id) => begin_as_user(pool, user_id).await,
        }
    }
}

/// `(user_id, org_id)` pair carried by tool / worker code paths.
///
/// Replaces the historical pattern of passing
/// `(acting_user_id, org_id, created_by_user_id)` as three separate
/// arguments — those values are definitionally equal (the worker
/// derives `acting_user_id` from `sessions.created_by_user_id`, child
/// sessions inherit the parent's `(org_id, created_by_user_id)`), so
/// one struct is both more honest and lighter at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caller {
    pub user_id: UserId,
    pub org_id: OrgId,
}

impl Caller {
    #[must_use]
    pub fn new(user_id: UserId, org_id: OrgId) -> Self {
        Self { user_id, org_id }
    }
}

/// Domain tables that a route may probe for tenant visibility. The
/// variant maps to a `&'static str` so the table name in the probe SQL
/// is never user-controlled (CLAUDE.md §10).
#[derive(Debug, Clone, Copy)]
pub enum VisibilityTable {
    Agents,
    McpServers,
    PromptRequests,
}

impl VisibilityTable {
    const fn table(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::McpServers => "mcp_servers",
            Self::PromptRequests => "prompt_requests",
        }
    }
}

/// Tenant-visibility probe shared by the HTTP routes.
///
/// Routes that delegate to a privileged-tx store must first 404
/// cross-org / unknown ids without leaking existence. Opens a
/// `begin_as` tx so RLS filters to rows the principal can see, runs
/// `SELECT EXISTS(SELECT 1 FROM <table> WHERE id = $1)`, commits, and
/// returns the boolean.
pub async fn visible_to(
    pool: &PgPool,
    principal: &Principal,
    table: VisibilityTable,
    id: uuid::Uuid,
) -> Result<bool, AuthError> {
    let mut tx = begin_as(pool, principal).await?;
    let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM {} WHERE id = $1)",
        table.table()
    );
    let exists: bool = sqlx::query_scalar(&sql)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(exists)
}
