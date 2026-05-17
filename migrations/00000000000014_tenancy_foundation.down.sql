-- Reverse migration 14.

DROP POLICY IF EXISTS mcp_servers_org_isolation ON mcp_servers;
DROP INDEX IF EXISTS mcp_servers_org_enabled_idx;
CREATE INDEX mcp_servers_enabled_idx
    ON mcp_servers (alias) WHERE enabled = TRUE;
ALTER TABLE mcp_servers DROP CONSTRAINT IF EXISTS mcp_servers_org_alias_key;
ALTER TABLE mcp_servers ADD CONSTRAINT mcp_servers_alias_key UNIQUE (alias);
ALTER TABLE mcp_servers DROP COLUMN IF EXISTS org_id;
ALTER TABLE mcp_servers DISABLE ROW LEVEL SECURITY;

DROP FUNCTION IF EXISTS app_user_is_member(UUID);
DROP FUNCTION IF EXISTS app_current_user_id();

DROP TABLE IF EXISTS oauth_login_states;
DROP TABLE IF EXISTS org_members;
DROP TABLE IF EXISTS organizations;
DROP TABLE IF EXISTS user_identities;
DROP TABLE IF EXISTS users;

-- Drop the RLS role last. REASSIGN OWNED + DROP OWNED handle any
-- dangling default privileges granted on this schema.
DO $$
DECLARE s text := current_schema();
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'relay_app') THEN
        EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA %I REVOKE ALL ON TABLES FROM relay_app', s);
        EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA %I REVOKE ALL ON FUNCTIONS FROM relay_app', s);
        EXECUTE format('REVOKE ALL ON ALL TABLES IN SCHEMA %I FROM relay_app', s);
        EXECUTE format('REVOKE ALL ON ALL FUNCTIONS IN SCHEMA %I FROM relay_app', s);
        EXECUTE format('REVOKE USAGE ON SCHEMA %I FROM relay_app', s);
    END IF;
END
$$;
