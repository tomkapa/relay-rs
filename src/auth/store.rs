//! `UserStore` — the only auth-side trait. Defines insert/lookup over
//! `users`, `user_identities`, `organizations`, `org_members`, and the
//! short-lived `oauth_login_states` rows.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::error::AuthError;
use super::types::{
    GoogleProfile, OAuthState, OrgId, OrgMembership, PkceVerifier, Role, User, UserId,
};

pub type SharedUserStore = Arc<dyn UserStore>;

/// New row to be inserted into `oauth_login_states`.
#[derive(Debug, Clone)]
pub struct OAuthStateRow {
    pub state: OAuthState,
    pub pkce_verifier: PkceVerifier,
    pub redirect_to: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Row returned when consuming a `oauth_login_states` row.
#[derive(Debug, Clone)]
pub struct ConsumedOAuthState {
    pub pkce_verifier: PkceVerifier,
    pub redirect_to: Option<String>,
}

/// Result of an OAuth upsert. `is_new_user` lets the caller branch on
/// "first sign-up, seed personal org" without an extra round trip.
#[derive(Debug, Clone)]
pub struct UpsertedUser {
    pub user: User,
    pub is_new_user: bool,
}

/// A freshly-created organisation row.
#[derive(Debug, Clone)]
pub struct NewOrg {
    pub id: OrgId,
    pub slug: String,
    pub name: String,
}

#[async_trait]
pub trait UserStore: std::fmt::Debug + Send + Sync + 'static {
    /// Insert or update the user + identity rows that map to one Google
    /// profile. Idempotent on `(provider, subject)`.
    async fn upsert_from_google(
        &self,
        profile: &GoogleProfile,
        now: DateTime<Utc>,
    ) -> Result<UpsertedUser, AuthError>;

    /// Create a personal organisation for a user. Returns the new org;
    /// also inserts an `org_members` row with role = Owner.
    async fn create_personal_org(
        &self,
        user_id: UserId,
        suggested_slug: &str,
        display_name: &str,
        now: DateTime<Utc>,
    ) -> Result<NewOrg, AuthError>;

    /// List every org the user belongs to.
    async fn list_user_orgs(&self, user_id: UserId) -> Result<Vec<OrgMembership>, AuthError>;

    /// Return the user's role in `org_id`, or `None` if they're not a
    /// member.
    async fn membership(&self, user_id: UserId, org_id: OrgId) -> Result<Option<Role>, AuthError>;

    /// Look up a user by id (for `/me`).
    async fn read_user(&self, user_id: UserId) -> Result<Option<User>, AuthError>;

    /// Insert a `oauth_login_states` row. Caller has minted the random
    /// `state` + PKCE verifier.
    async fn insert_oauth_state(&self, row: &OAuthStateRow) -> Result<(), AuthError>;

    /// Atomically consume an `oauth_login_states` row by `state`. Deletes
    /// the row on success and returns the stored verifier; returns
    /// [`AuthError::OAuthStateInvalid`] when the row is missing or
    /// expired.
    async fn consume_oauth_state(
        &self,
        state: &OAuthState,
        now: DateTime<Utc>,
    ) -> Result<ConsumedOAuthState, AuthError>;
}
