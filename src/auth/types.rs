//! Identity-surface newtypes. Per CLAUDE.md §1 every value carrying an
//! invariant is a newtype with a `TryFrom` smart constructor.

use std::sync::Arc;

use crate::types::ParseError;

crate::uuid_newtype! {
    /// Opaque identifier for a user row in `users`.
    pub UserId
}

crate::uuid_newtype! {
    /// Opaque identifier for an organization row.
    pub OrgId
}

crate::str_enum! {
    /// Role of a user within one org. Used by `org_members.role` and the
    /// JWT membership lookup.
    pub enum Role {
        Owner  => "owner",
        Admin  => "admin",
        Member => "member",
    }
}

/// RFC-ish email address.
///
/// We don't try to be a full RFC 5321 parser — just guard length and
/// the "must contain `@`" shape that downstream code relies on. The
/// Postgres `citext` column does the case-insensitive uniqueness work.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Email(Arc<str>);

impl Email {
    pub const MAX_BYTES: usize = 320;
    pub const MIN_BYTES: usize = 3;

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Email {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Email").field(&self.as_str()).finish()
    }
}

impl std::fmt::Display for Email {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<&str> for Email {
    type Error = ParseError;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(ParseError::Empty { field: "email" });
        }
        if trimmed.len() < Self::MIN_BYTES {
            return Err(ParseError::OutOfRange {
                field: "email",
                detail: "too short",
            });
        }
        if trimmed.len() > Self::MAX_BYTES {
            return Err(ParseError::TooLong {
                field: "email",
                max: Self::MAX_BYTES,
                got: trimmed.len(),
            });
        }
        // The `@` check is enough to refuse the obvious junk; deeper
        // validation belongs to the OAuth provider.
        let at = trimmed.find('@').ok_or(ParseError::Malformed {
            field: "email",
            detail: "missing @",
        })?;
        if at == 0 || at == trimmed.len() - 1 {
            return Err(ParseError::Malformed {
                field: "email",
                detail: "missing local or domain part",
            });
        }
        Ok(Self(Arc::from(trimmed)))
    }
}

impl TryFrom<String> for Email {
    type Error = ParseError;
    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::try_from(raw.as_str())
    }
}

/// URL-safe organization slug. Mirrors the migration's CHECK regex:
/// `^[a-z0-9][a-z0-9-]{0,62}$`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct OrgSlug(Arc<str>);

impl OrgSlug {
    pub const MAX_BYTES: usize = 63;

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for OrgSlug {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("OrgSlug").field(&self.as_str()).finish()
    }
}

impl std::fmt::Display for OrgSlug {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<&str> for OrgSlug {
    type Error = ParseError;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        if raw.is_empty() {
            return Err(ParseError::Empty { field: "org_slug" });
        }
        if raw.len() > Self::MAX_BYTES {
            return Err(ParseError::TooLong {
                field: "org_slug",
                max: Self::MAX_BYTES,
                got: raw.len(),
            });
        }
        let mut chars = raw.chars();
        let first = chars
            .next()
            .ok_or(ParseError::Empty { field: "org_slug" })?;
        if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
            return Err(ParseError::Malformed {
                field: "org_slug",
                detail: "must start with [a-z0-9]",
            });
        }
        for ch in chars {
            let ok = ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-';
            if !ok {
                return Err(ParseError::Malformed {
                    field: "org_slug",
                    detail: "only [a-z0-9-] after the first char",
                });
            }
        }
        Ok(Self(Arc::from(raw)))
    }
}

impl TryFrom<String> for OrgSlug {
    type Error = ParseError;
    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::try_from(raw.as_str())
    }
}

/// Profile claims pulled from Google's `userinfo` endpoint. Built by
/// [`crate::auth::oauth_google`] and consumed by [`UserStore::upsert_from_google`].
#[derive(Debug, Clone)]
pub struct GoogleProfile {
    pub subject: String,
    pub email: Email,
    pub email_verified: bool,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

/// Materialised user row.
#[derive(Debug, Clone)]
pub struct User {
    pub id: UserId,
    pub email: Email,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

/// One row from `org_members` joined with `organizations`.
#[derive(Debug, Clone)]
pub struct OrgMembership {
    pub org_id: OrgId,
    pub org_name: String,
    pub org_slug: OrgSlug,
    pub role: Role,
}

/// What every authed HTTP request hands to its handler. Built by the
/// [`crate::http::auth_layer`] middleware from the JWT cookie + a DB
/// membership lookup.
#[derive(Debug, Clone)]
pub struct Principal {
    pub user_id: UserId,
    pub active_org_id: OrgId,
    pub role: Role,
}
