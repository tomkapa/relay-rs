//! Google OAuth login / callback. Public — the auth layer is not
//! applied here (the whole point is to mint a session for an as-yet
//! unauthenticated visitor).
//!
//! Flow:
//!   1. `GET  /auth/google/login?return_to=…` — mint random `state` +
//!      PKCE verifier, insert into `oauth_login_states`, redirect to
//!      Google.
//!   2. `GET  /auth/google/callback?code=…&state=…` — consume the
//!      stored verifier, exchange the code, upsert the user (and
//!      create their personal org on first sign-up), mint a JWT, set
//!      the session cookie, redirect to the validated `return_to` (or
//!      `/`).

use axum::Router;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::{DateTime, Utc};
use cookie::time::Duration as CookieDuration;
use serde::Deserialize;
use tracing::info;

use crate::auth::{OAuthStateRow, limits::COOKIE_NAME};

use super::super::error::HttpError;
use super::super::state::AppState;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/google/login", get(login))
        .route("/auth/google/callback", get(callback))
}

#[derive(Debug, Deserialize)]
struct LoginQuery {
    #[serde(default)]
    return_to: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: String,
    state: String,
    /// Google may return `error=access_denied` when the user clicks
    /// "cancel". Surface as a 400 so the FE can react.
    #[serde(default)]
    error: Option<String>,
}

async fn login(
    State(state): State<AppState>,
    Query(query): Query<LoginQuery>,
) -> Result<Redirect, HttpError> {
    let start = state.oauth.start();
    let now = now_utc(&state);
    let expires = now
        + chrono::Duration::from_std(crate::auth::limits::OAUTH_STATE_TTL)
            .unwrap_or_else(|_| chrono::Duration::seconds(600));
    let safe_return = query.return_to.and_then(|r| sanitize_return_to(&r));
    state
        .users
        .insert_oauth_state(&OAuthStateRow {
            state: start.state.clone(),
            pkce_verifier: start.pkce_verifier.clone(),
            redirect_to: safe_return,
            created_at: now,
            expires_at: expires,
        })
        .await?;
    Ok(Redirect::to(start.authorize_url.as_str()))
}

async fn callback(
    State(state): State<AppState>,
    Query(query): Query<CallbackQuery>,
    jar: CookieJar,
) -> Result<Response, HttpError> {
    if let Some(err) = query.error.as_deref() {
        return Err(HttpError::BadRequest(format!("oauth: {err}")));
    }
    let now = now_utc(&state);
    let consumed = state.users.consume_oauth_state(&query.state, now).await?;

    let profile = state
        .oauth
        .exchange(&query.code, &consumed.pkce_verifier)
        .await?;
    let upserted = state.users.upsert_from_google(&profile, now).await?;

    // First sign-up → mint a personal org so the user has somewhere to
    // own resources. The default-agent seed for the new org is the
    // composition root's job (see `app::seed_default_agent_for_org`)
    // and runs here so the cookie we mint immediately resolves to a
    // usable workspace.
    let orgs = state.users.list_user_orgs(upserted.user.id).await?;
    let active_org = if let Some(first) = orgs.first() {
        first.org_id
    } else {
        let display = upserted
            .user
            .display_name
            .clone()
            .unwrap_or_else(|| upserted.user.email.as_str().to_owned());
        let slug_seed = upserted
            .user
            .email
            .as_str()
            .split('@')
            .next()
            .unwrap_or("workspace");
        let new_org = state
            .users
            .create_personal_org(upserted.user.id, slug_seed, &display, now)
            .await?;
        // Seed the freshly minted personal org's default agent so the
        // cookie we mint below immediately resolves to a usable
        // workspace. Idempotent: re-running this for a pre-existing org
        // returns the existing default's id rather than minting another.
        // `default_agent_seed` only fails if the seed constants violate
        // a newtype invariant — surface as an internal error rather than
        // a user-facing 4xx (it's a server-side bug, not a bad request).
        let seed = crate::app::default_agent_seed().map_err(|e| {
            tracing::error!(error = %e, "auth.callback.default_agent_seed_build_failed");
            HttpError::Internal
        })?;
        crate::app::seed_default_agent_for_org(&state.agents, new_org.id, seed).await?;
        new_org.id
    };

    let token = state.jwt.mint(upserted.user.id, active_org)?;
    info!(
        user.id = %upserted.user.id,
        org.id = %active_org,
        new_user = upserted.is_new_user,
        "auth.login.success"
    );

    let cookie = build_session_cookie(token, state.cookie_secure());
    let jar = jar.add(cookie);
    let dest = consumed.redirect_to.unwrap_or_else(|| "/".to_owned());
    // axum-extra: tupling a CookieJar with a Redirect produces a
    // response that carries both `Set-Cookie` and `Location` headers.
    Ok((jar, Redirect::to(&dest)).into_response())
}

fn build_session_cookie(token: String, secure: bool) -> Cookie<'static> {
    let mut cookie = Cookie::new(COOKIE_NAME, token);
    cookie.set_http_only(true);
    cookie.set_path("/");
    cookie.set_same_site(SameSite::Lax);
    cookie.set_secure(secure);
    cookie.set_max_age(CookieDuration::seconds(
        i64::try_from(crate::auth::limits::JWT_TTL.as_secs()).unwrap_or(i64::MAX),
    ));
    cookie
}

fn now_utc(state: &AppState) -> DateTime<Utc> {
    DateTime::<Utc>::from(state.clock.now_wall())
}

/// Only allow relative-path return URLs (no scheme, no host) so the
/// callback can't be turned into an open-redirect to attacker domains.
fn sanitize_return_to(raw: &str) -> Option<String> {
    if raw.starts_with('/') && !raw.starts_with("//") && raw.len() <= 2048 {
        Some(raw.to_owned())
    } else {
        None
    }
}
