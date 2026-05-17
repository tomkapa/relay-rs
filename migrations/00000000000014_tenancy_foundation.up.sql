-- Tenancy foundation (doc/task1.md §80, see plan
-- /Users/tomtran/.claude/plans/tenancy-foundation-goal-every-refactored-marshmallow.md).
--
-- Lands the identity surface (users, organizations, …), the RLS plumbing
-- (`app_current_user_id` / `app_user_is_member` helpers), and the
-- end-to-end isolation probe on `mcp_servers`. The remaining domain
-- tables (`agents`, `sessions`, `prompt_requests`, `memory_*`,
-- `scheduled_tasks`, …) are reserved for a follow-up migration that
-- will pair each `org_id` column with the corresponding store retrofit
-- so the build never goes red mid-PR.
--
-- Pre-launch: NOT NULL columns are added with no backfill; existing dev
-- rows must be wiped before applying. See `feedback_no_backcompat`.

CREATE EXTENSION IF NOT EXISTS citext;

-- ───────────────────────────────────────────────────────────────────────────
-- Identity surface.
-- ───────────────────────────────────────────────────────────────────────────

CREATE TABLE users (
    id           UUID PRIMARY KEY,
    email        CITEXT NOT NULL UNIQUE
                 CHECK (octet_length(email) BETWEEN 3 AND 320),
    display_name TEXT CHECK (display_name IS NULL OR octet_length(display_name) <= 200),
    avatar_url   TEXT CHECK (avatar_url IS NULL OR octet_length(avatar_url) <= 2048),
    created_at   TIMESTAMPTZ NOT NULL,
    updated_at   TIMESTAMPTZ NOT NULL
);

-- One row per (provider, subject). Google = ('google', sub-claim).
-- Lets us bolt on GitHub / SSO later without schema work.
CREATE TABLE user_identities (
    user_id       UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider      TEXT NOT NULL CHECK (provider IN ('google')),
    subject       TEXT NOT NULL CHECK (octet_length(subject) BETWEEN 1 AND 255),
    email_at_link CITEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (provider, subject)
);
CREATE INDEX user_identities_user_idx ON user_identities (user_id);

CREATE TABLE organizations (
    id         UUID PRIMARY KEY,
    name       TEXT  NOT NULL CHECK (octet_length(name) BETWEEN 1 AND 200),
    slug       CITEXT NOT NULL UNIQUE
               CHECK (slug ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE org_members (
    org_id     UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role       TEXT NOT NULL CHECK (role IN ('owner','admin','member')),
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (org_id, user_id)
);
CREATE INDEX org_members_user_idx ON org_members (user_id);

-- OAuth login state. Short-lived row keyed by the `state` param so the
-- callback can verify CSRF + recover the PKCE verifier.
CREATE TABLE oauth_login_states (
    state         TEXT PRIMARY KEY CHECK (octet_length(state) BETWEEN 32 AND 128),
    pkce_verifier TEXT NOT NULL CHECK (octet_length(pkce_verifier) BETWEEN 32 AND 128),
    redirect_to   TEXT CHECK (redirect_to IS NULL OR octet_length(redirect_to) <= 2048),
    created_at    TIMESTAMPTZ NOT NULL,
    expires_at    TIMESTAMPTZ NOT NULL
);
CREATE INDEX oauth_login_states_expires_idx ON oauth_login_states (expires_at);

-- ───────────────────────────────────────────────────────────────────────────
-- RLS helpers. STABLE so the planner can short-circuit; SECURITY INVOKER
-- (the default) so they read the *caller's* GUC, not the function owner's.
-- ───────────────────────────────────────────────────────────────────────────

CREATE OR REPLACE FUNCTION app_current_user_id() RETURNS UUID
    LANGUAGE sql STABLE AS $$
    SELECT NULLIF(current_setting('app.user_id', true), '')::UUID
$$;

CREATE OR REPLACE FUNCTION app_user_is_member(target_org UUID) RETURNS BOOLEAN
    LANGUAGE sql STABLE AS $$
    SELECT EXISTS (
        SELECT 1 FROM org_members m
         WHERE m.user_id = app_current_user_id()
           AND m.org_id  = target_org
    )
$$;

-- ───────────────────────────────────────────────────────────────────────────
-- RLS-enforcing role.
--
-- The default app role (`relay` in dev / tests) is a superuser, and
-- Postgres superusers bypass RLS unconditionally — even with
-- FORCE ROW LEVEL SECURITY. To make tenant-scoped requests actually
-- subject to policies we drop privileges per-transaction via
-- `SET LOCAL ROLE relay_app` inside `auth::begin_as`. Granting CRUD on
-- the relevant tables to this role lets the queries succeed; the policy
-- then provides the isolation.
-- ───────────────────────────────────────────────────────────────────────────

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'relay_app') THEN
        CREATE ROLE relay_app NOLOGIN;
    END IF;
END
$$;

-- Grant CRUD on every existing + future table in the *current* schema
-- (the migration runs once per schema-isolated test DB, so dynamic SQL
-- with `current_schema()` keeps the grants schema-correct without us
-- hard-coding `public`).
DO $$
DECLARE s text := current_schema();
BEGIN
    EXECUTE format('GRANT USAGE ON SCHEMA %I TO relay_app', s);
    EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA %I TO relay_app', s);
    EXECUTE format('GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA %I TO relay_app', s);
    EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA %I GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO relay_app', s);
    EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA %I GRANT EXECUTE ON FUNCTIONS TO relay_app', s);
END
$$;

-- Public-schema USAGE so the role can resolve types defined by
-- extensions installed in `public` (the `vector` type from pgvector,
-- the `citext` type from citext). Without this, an `INSERT` whose
-- parameter list references `$N::vector` fails at parse time with
-- "type vector does not exist" when the connection's role is
-- `relay_app`. The grant is global to the role, not per-schema, so
-- it survives across the test-DB's many schemas.
GRANT USAGE ON SCHEMA public TO relay_app;

-- ───────────────────────────────────────────────────────────────────────────
-- mcp_servers: full org_id retrofit + RLS as the end-to-end probe.
--
-- The other domain tables get `org_id` (nullable, no RLS yet) in a
-- follow-up migration paired with their store retrofits so the build
-- never goes red mid-PR. The pattern is identical: add the column,
-- ENABLE/FORCE RLS, create a `<tbl>_org_isolation` policy.
-- ───────────────────────────────────────────────────────────────────────────

ALTER TABLE mcp_servers
    ADD COLUMN org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE;
ALTER TABLE mcp_servers DROP CONSTRAINT mcp_servers_alias_key;
ALTER TABLE mcp_servers ADD CONSTRAINT mcp_servers_org_alias_key UNIQUE (org_id, alias);
DROP INDEX mcp_servers_enabled_idx;
CREATE INDEX mcp_servers_org_enabled_idx
    ON mcp_servers (org_id, alias) WHERE enabled = TRUE;
ALTER TABLE mcp_servers ENABLE ROW LEVEL SECURITY;
ALTER TABLE mcp_servers FORCE ROW LEVEL SECURITY;
CREATE POLICY mcp_servers_org_isolation ON mcp_servers
    FOR ALL TO PUBLIC
    USING      (app_user_is_member(org_id))
    WITH CHECK (app_user_is_member(org_id));
