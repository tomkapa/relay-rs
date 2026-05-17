//! Postgres-backed [`UserStore`]. Identity tables are not RLS-protected
//! in this PR (every authenticated request can see at least their own
//! user/org rows via the `/me` route). Mutations still go through
//! [`super::begin_privileged`] so we can extend RLS to these tables
//! later without rewriting this module.

use std::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use sqlx::Row;

use super::error::AuthError;
use super::limits::MAX_SLUG_RETRIES;
use super::store::{ConsumedOAuthState, NewOrg, OAuthStateRow, UpsertedUser, UserStore};
use super::types::{
    Email, GoogleProfile, OAuthState, OrgId, OrgMembership, OrgSlug, PkceVerifier, Role, User,
    UserId,
};

pub struct PgUserStore {
    pool: PgPool,
}

impl PgUserStore {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl fmt::Debug for PgUserStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PgUserStore").finish_non_exhaustive()
    }
}

#[async_trait]
impl UserStore for PgUserStore {
    async fn upsert_from_google(
        &self,
        profile: &GoogleProfile,
        now: DateTime<Utc>,
    ) -> Result<UpsertedUser, AuthError> {
        let mut tx = super::begin_privileged(&self.pool).await?;

        // Idempotent under concurrent first-logins. Two callbacks for
        // the same brand-new Google account otherwise race on the
        // read-then-insert sequence; one would 23505 on the unique
        // constraint and the sign-in would 500 with stale state in the
        // partial. The `ON CONFLICT … RETURNING id` pattern collapses
        // the "insert or get existing" decision into one statement that
        // either ways yields the canonical id.
        let candidate_id = UserId::new();
        let user_id: UserId = sqlx::query_scalar(
            "INSERT INTO users (id, email, display_name, avatar_url, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $5)
             ON CONFLICT (email) DO UPDATE
                 SET display_name = EXCLUDED.display_name,
                     avatar_url   = EXCLUDED.avatar_url,
                     updated_at   = EXCLUDED.updated_at
             RETURNING id",
        )
        .bind(candidate_id)
        .bind(profile.email.as_str())
        .bind(profile.display_name.as_deref())
        .bind(profile.avatar_url.as_deref())
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        // is_new = true iff the INSERT actually used our candidate id.
        let is_new_user = user_id.as_uuid() == candidate_id.as_uuid();

        sqlx::query(
            "INSERT INTO user_identities (user_id, provider, subject, email_at_link, created_at)
             VALUES ($1, 'google', $2, $3, $4)
             ON CONFLICT (provider, subject) DO NOTHING",
        )
        .bind(user_id)
        .bind(profile.subject.as_str())
        .bind(profile.email.as_str())
        .bind(now)
        .execute(&mut *tx)
        .await?;

        // Read back the canonical user row.
        let row =
            sqlx::query("SELECT id, email, display_name, avatar_url FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_one(&mut *tx)
                .await?;
        tx.commit().await?;

        let user = User {
            id: user_id,
            email: Email::try_from(row.get::<String, _>("email"))?,
            display_name: row.get("display_name"),
            avatar_url: row.get("avatar_url"),
        };
        Ok(UpsertedUser { user, is_new_user })
    }

    async fn create_personal_org(
        &self,
        user_id: UserId,
        suggested_slug: &str,
        display_name: &str,
        now: DateTime<Utc>,
    ) -> Result<NewOrg, AuthError> {
        let base = sanitize_slug(suggested_slug);
        let mut attempt = 0;
        loop {
            let candidate = if attempt == 0 {
                base.clone()
            } else {
                format!("{base}-{}", random_suffix())
            };
            // Re-parse through OrgSlug to make sure we don't insert a row
            // the CHECK constraint would reject.
            let slug = match OrgSlug::try_from(candidate.as_str()) {
                Ok(s) => s,
                Err(_) if attempt < MAX_SLUG_RETRIES => {
                    attempt += 1;
                    continue;
                }
                Err(e) => return Err(AuthError::Parse(e)),
            };
            match self
                .try_insert_org(user_id, slug.as_str(), display_name, now)
                .await
            {
                Ok(new_org) => return Ok(new_org),
                Err(AuthError::Db(sqlx::Error::Database(db)))
                    if db.code().as_deref() == Some("23505") =>
                {
                    if attempt >= MAX_SLUG_RETRIES {
                        return Err(AuthError::Internal("could not mint unique org slug"));
                    }
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn list_user_orgs(&self, user_id: UserId) -> Result<Vec<OrgMembership>, AuthError> {
        let mut tx = super::begin_privileged(&self.pool).await?;
        let rows = sqlx::query(
            "SELECT o.id, o.name, o.slug::text AS slug, m.role
             FROM org_members m
             JOIN organizations o ON o.id = m.org_id
             WHERE m.user_id = $1
             ORDER BY o.created_at ASC",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.into_iter()
            .map(|r| {
                Ok(OrgMembership {
                    org_id: OrgId::from(r.get::<uuid::Uuid, _>("id")),
                    org_name: r.get("name"),
                    org_slug: OrgSlug::try_from(r.get::<String, _>("slug"))?,
                    role: Role::parse(r.get::<&str, _>("role"))
                        .ok_or(AuthError::Internal("unknown role in db"))?,
                })
            })
            .collect()
    }

    async fn membership(&self, user_id: UserId, org_id: OrgId) -> Result<Option<Role>, AuthError> {
        let mut tx = super::begin_privileged(&self.pool).await?;
        let row: Option<String> =
            sqlx::query_scalar("SELECT role FROM org_members WHERE user_id = $1 AND org_id = $2")
                .bind(user_id)
                .bind(org_id)
                .fetch_optional(&mut *tx)
                .await?;
        tx.commit().await?;
        Ok(row.and_then(|r| Role::parse(&r)))
    }

    async fn read_user(&self, user_id: UserId) -> Result<Option<User>, AuthError> {
        let mut tx = super::begin_privileged(&self.pool).await?;
        let row =
            sqlx::query("SELECT id, email, display_name, avatar_url FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_optional(&mut *tx)
                .await?;
        tx.commit().await?;
        let Some(r) = row else { return Ok(None) };
        Ok(Some(User {
            id: UserId::from(r.get::<uuid::Uuid, _>("id")),
            email: Email::try_from(r.get::<String, _>("email"))?,
            display_name: r.get("display_name"),
            avatar_url: r.get("avatar_url"),
        }))
    }

    async fn insert_oauth_state(&self, row: &OAuthStateRow) -> Result<(), AuthError> {
        let mut tx = super::begin_privileged(&self.pool).await?;
        sqlx::query(
            "INSERT INTO oauth_login_states (state, pkce_verifier, redirect_to, created_at, expires_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(row.state.as_str())
        .bind(row.pkce_verifier.as_str())
        .bind(row.redirect_to.as_deref())
        .bind(row.created_at)
        .bind(row.expires_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn consume_oauth_state(
        &self,
        state: &OAuthState,
        now: DateTime<Utc>,
    ) -> Result<ConsumedOAuthState, AuthError> {
        let mut tx = super::begin_privileged(&self.pool).await?;
        let row = sqlx::query(
            "DELETE FROM oauth_login_states
             WHERE state = $1 AND expires_at > $2
             RETURNING pkce_verifier, redirect_to",
        )
        .bind(state.as_str())
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        // Best-effort cleanup of expired rows on every consume. Bounded
        // by the row count; this table is small (10 min TTL).
        sqlx::query("DELETE FROM oauth_login_states WHERE expires_at <= $1")
            .bind(now)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let row = row.ok_or(AuthError::OAuthStateInvalid)?;
        Ok(ConsumedOAuthState {
            pkce_verifier: PkceVerifier::try_from(row.get::<String, _>("pkce_verifier"))?,
            redirect_to: row.get("redirect_to"),
        })
    }
}

impl PgUserStore {
    async fn try_insert_org(
        &self,
        user_id: UserId,
        slug: &str,
        display_name: &str,
        now: DateTime<Utc>,
    ) -> Result<NewOrg, AuthError> {
        let mut tx = super::begin_privileged(&self.pool).await?;
        let id = OrgId::new();
        sqlx::query(
            "INSERT INTO organizations (id, name, slug, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $4)",
        )
        .bind(id)
        .bind(display_name)
        .bind(slug)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO org_members (org_id, user_id, role, created_at)
             VALUES ($1, $2, 'owner', $3)",
        )
        .bind(id)
        .bind(user_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(NewOrg {
            id,
            slug: slug.to_owned(),
            name: display_name.to_owned(),
        })
    }
}

fn sanitize_slug(raw: &str) -> String {
    // Drop everything that isn't `[a-z0-9-]`; collapse runs of `-`;
    // strip leading non-alphanumerics; cap at 50 chars to leave room
    // for the random suffix on collision.
    let mut out = String::with_capacity(raw.len());
    let lower = raw.to_lowercase();
    let mut last_dash = false;
    for ch in lower.chars() {
        let ok = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        if ok {
            out.push(ch);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("org");
    }
    if out.len() > 50 {
        out.truncate(50);
        while out.ends_with('-') {
            out.pop();
        }
    }
    out
}

fn random_suffix() -> String {
    // 4-char random suffix from uuid; cheap, no extra dep needed.
    uuid::Uuid::new_v4().simple().to_string()[..4].to_owned()
}
