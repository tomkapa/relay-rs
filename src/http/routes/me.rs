//! Authenticated identity endpoints. Mounted under the auth layer so
//! every handler here receives a [`Principal`].

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use cookie::time::Duration as CookieDuration;
use serde::{Deserialize, Serialize};

use crate::auth::{
    AuthError, OrgId, OrgMembership, Principal, Role, User,
    limits::{COOKIE_NAME, CSRF_COOKIE_NAME, CSRF_TOKEN_MAX_LEN},
};

use super::super::csrf::{build_csrf_cookie, build_expired_csrf_cookie, mint_csrf_token};
use super::super::error::HttpError;
use super::super::state::AppState;
use super::auth::build_session_cookie;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/me", get(me))
        .route("/auth/logout", post(logout))
        .route("/auth/switch-org", post(switch_org))
}

#[derive(Debug, Serialize)]
struct MeResponse {
    user: UserView,
    orgs: Vec<OrgView>,
    active_org_id: OrgId,
    role: Role,
}

#[derive(Debug, Serialize)]
struct UserView {
    id: crate::auth::UserId,
    email: String,
    display_name: Option<String>,
    avatar_url: Option<String>,
}

#[derive(Debug, Serialize)]
struct OrgView {
    id: OrgId,
    name: String,
    slug: String,
    role: Role,
}

async fn me(
    State(state): State<AppState>,
    principal: Principal,
    jar: CookieJar,
) -> Result<Response, HttpError> {
    let user = state
        .users
        .read_user(principal.user_id)
        .await?
        .ok_or(AuthError::Unauthenticated)?;
    let orgs = state.users.list_user_orgs(principal.user_id).await?;
    // Only mint a CSRF cookie if the client doesn't already have a
    // valid one — /me is polled, and rotating the token every poll
    // defeats the SPA's cached value and bloats every response with a
    // Set-Cookie. Mirror require_csrf's accept-band so a malformed
    // cookie gets repaired instead of locking the session out of
    // every POST.
    let needs_csrf = jar.get(CSRF_COOKIE_NAME).is_none_or(|c| {
        let bytes = c.value().as_bytes();
        bytes.is_empty() || bytes.len() > CSRF_TOKEN_MAX_LEN
    });
    let response_jar = if needs_csrf {
        jar.add(build_csrf_cookie(
            mint_csrf_token(),
            state.cookie_secure(),
            state.jwt.ttl_secs(),
        ))
    } else {
        jar
    };
    let body = Json(MeResponse {
        user: view_user(user),
        orgs: orgs.iter().map(view_org).collect(),
        active_org_id: principal.active_org_id,
        role: principal.role,
    });
    Ok((response_jar, body).into_response())
}

fn view_user(u: User) -> UserView {
    UserView {
        id: u.id,
        email: u.email.as_str().to_owned(),
        display_name: u.display_name,
        avatar_url: u.avatar_url,
    }
}

fn view_org(m: &OrgMembership) -> OrgView {
    OrgView {
        id: m.org_id,
        name: m.org_name.clone(),
        slug: m.org_slug.as_str().to_owned(),
        role: m.role,
    }
}

async fn logout(State(state): State<AppState>, jar: CookieJar) -> Response {
    let mut cookie = Cookie::new(COOKIE_NAME, "");
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_secure(state.cookie_secure());
    cookie.set_max_age(CookieDuration::seconds(0));
    let jar = jar
        .add(cookie)
        .add(build_expired_csrf_cookie(state.cookie_secure()));
    (StatusCode::NO_CONTENT, jar).into_response()
}

#[derive(Debug, Deserialize)]
struct SwitchOrgRequest {
    org_id: OrgId,
}

async fn switch_org(
    State(state): State<AppState>,
    principal: Principal,
    jar: CookieJar,
    Json(req): Json<SwitchOrgRequest>,
) -> Result<Response, HttpError> {
    let role = state
        .users
        .membership(principal.user_id, req.org_id)
        .await?
        .ok_or(AuthError::NotMember(req.org_id))?;
    let token = state.jwt.mint(principal.user_id, req.org_id)?;
    let session_cookie = build_session_cookie(token, state.cookie_secure(), state.jwt.ttl_secs());
    let csrf_cookie = build_csrf_cookie(
        mint_csrf_token(),
        state.cookie_secure(),
        state.jwt.ttl_secs(),
    );
    let jar = jar.add(session_cookie).add(csrf_cookie);
    Ok((
        jar,
        Json(serde_json::json!({ "active_org_id": req.org_id, "role": role })),
    )
        .into_response())
}
