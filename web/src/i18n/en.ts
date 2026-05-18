// English string table.
//
// Keep keys flat and namespaced (`area.subarea.role`). `{name}`-style
// placeholders are supported via the lightweight in-tree interpolator
// in `index.ts::t()` — pass values via `t("key", { name: "…" })`. No
// runtime i18n dep (CLAUDE.md §8); promote only when ICU plurals are
// actually needed.
//
// Every key in this file MUST exist in `vi.ts`. The startup-time check in
// `index.ts` panics if they drift.

const en = {
  "signin.brand": "Relay",
  "signin.tagline": "The operator console for multi-agent sessions.",
  "signin.heading": "Sign in to Relay",
  "signin.subheading": "Continue with your Google account.",
  "signin.cta": "Continue with Google",
  "signin.legal": "By continuing, you agree to the Terms of Service and the Privacy Policy.",
  "signin.error.forbidden": "Access denied.",
  "signin.error.oauth_down": "Sign-in is temporarily unavailable. Please try again.",

  "sidebar.brand": "Relay",
  "sidebar.channels": "Channels",
  "sidebar.dms": "Direct Messages",
  "sidebar.empty_agents": "No agents registered.",

  "usermenu.signout": "Sign out",
  "usermenu.language.label": "Language",
  "usermenu.language.en": "English",
  "usermenu.language.vi": "Tiếng Việt",
  "usermenu.language.error": "Could not change language. Please try again.",

  "button.loading": "Loading",
  "time.justNow": "just now",
  "time.ago": "{value} ago",

  // ─── Connections ─────────────────────────────────────────────────
  "menu.connections": "Connections",
  "connections.workspace.label": "ACME WORKSPACE",
  "connections.nav.title": "Connections",
  "connections.nav.browse": "Browse catalog",
  "connections.nav.my": "My connections",
  "connections.breadcrumb.workspace": "Workspace",
  "connections.breadcrumb.connections": "Connections",
  "connections.breadcrumb.my": "My connections",
  "connections.breadcrumb.add": "Add connection",
  "connections.list.title": "My connections",
  "connections.list.subtitle":
    "Wired-up tools available to your workspace. Admins wire — agent owners allow.",
  "connections.list.add": "Add connection",
  "connections.list.filter": "All statuses",
  "connections.list.empty.title": "No connections yet",
  "connections.list.empty.body":
    "Browse the catalog to wire up Notion, Linear, GitHub, and more.",
  "connections.list.empty.cta": "Browse catalog",
  "connections.list.col.status": "STATUS",
  "connections.list.col.connection": "CONNECTION",
  "connections.list.col.owner": "OWNER",
  "connections.list.col.tools": "TOOLS",
  "connections.list.col.lastSeen": "LAST ACTIVE",
  "connections.list.col.enable": "ENABLE",
  "connections.status.ok": "Healthy",
  "connections.status.reconnect": "Reconnect",
  "connections.status.error": "Error",
  "connections.status.pending": "Pending",
  "connections.row.reconnect": "Reconnect",
  "connections.row.disconnect": "Disconnect",
  "connections.row.remove": "Remove",
  "connections.row.never": "never",
  "connections.row.toolsSuffix": "tools",
  "connections.catalog.title": "Add a connection",
  "connections.catalog.subtitle":
    "Connect Notion, Linear, GitHub, and more so your agents can act on real data.",
  "connections.catalog.search": "Search providers",
  "connections.catalog.tabs.all": "All",
  "connections.catalog.tabs.productivity": "Productivity",
  "connections.catalog.tabs.dev": "Developer tools",
  "connections.catalog.tabs.comms": "Communications",
  "connections.catalog.tabs.data": "Data",
  "connections.catalog.tabs.custom": "Custom",
  "connections.catalog.sort": "Sort:",
  "connections.catalog.sort.mostUsed": "Most used",
  "connections.catalog.tool": "tool",
  "connections.catalog.tools": "tools",
  "connections.catalog.added": "Already added",
  "connections.catalog.custom.title": "+ Custom",
  "connections.catalog.custom.blurb": "Paste an MCP server URL.",
  "connections.catalog.empty": "No providers match your search.",
  "connections.modal.cancel": "Cancel",
  "connections.modal.close": "Close",
  "connections.modal.oauth.eyebrow": "Connect",
  "connections.modal.oauth.title": "Connect {name} to your workspace",
  "connections.modal.oauth.bullet1":
    "Available to agents in your workspace once an admin enables it on the agent.",
  "connections.modal.oauth.bullet2": "You can disconnect at any time.",
  "connections.modal.oauth.bullet3":
    "{name} will ask you to choose which pages and databases to share.",
  "connections.modal.oauth.continue": "Continue to {name}",
  "connections.modal.token.tokenLabel": "{name} API token",
  "connections.modal.token.help": "Where do I find this?",
  "connections.modal.token.placeholder": "Paste your {name} API token",
  "connections.modal.token.note":
    "Stored encrypted. Never displayed after save.",
  "connections.modal.token.connect": "Connect",
  "connections.modal.token.testing": "Testing connection…",
  "connections.modal.token.error":
    "Couldn't reach {name}. Check the token and try again.",
  "connections.modal.reconnect.eyebrow": "Reconnect",
  "connections.modal.reconnect.title": "Reconnect {name}",
  "connections.modal.reconnect.alertTitle": "{name}'s permission expired",
  "connections.modal.reconnect.alertBody":
    "Token refresh failed. Reconnect to restore access.",
  "connections.modal.reconnect.lastSeen": "Last successful call",
  "connections.modal.reconnect.upstream": "Upstream response",
  "connections.modal.reconnect.notNow": "Not now",
  "connections.modal.reconnect.cta": "Reconnect to {name}",
  "connections.modal.custom.eyebrow": "Admin · Custom server",
  "connections.modal.custom.title": "Add a custom server",
  "connections.modal.custom.nameLabel": "Display name",
  "connections.modal.custom.namePlaceholder": "internal-search",
  "connections.modal.custom.urlLabel": "MCP server URL",
  "connections.modal.custom.urlHint":
    "http:// is rejected. URL must be reachable from Relay workers.",
  "connections.modal.custom.authLabel": "Authentication",
  "connections.modal.custom.authNone": "None",
  "connections.modal.custom.authToken": "API token",
  "connections.modal.custom.warn":
    "Any agent your admins enable will be able to call this URL with no credentials. Only use this for trusted internal servers.",
  "connections.modal.custom.cta": "Add server",
  "connections.modal.custom.error.alias":
    "Use lowercase letters, digits, _ or -, up to 16 characters.",
  "connections.modal.custom.error.url":
    "Enter a valid https:// URL.",
  "connections.callback.eyebrow.connecting": "OAuth · Step 2 of 3",
  "connections.callback.eyebrow.authorized": "OAuth · Step 3 of 3",
  "connections.callback.eyebrow.failed": "OAuth · Failed",
  "connections.callback.connecting.title": "Asking {name} to share access…",
  "connections.callback.connecting.body":
    "You should see {name}'s consent screen in a new tab.",
  "connections.callback.authorized.title": "{name} connected.",
  "connections.callback.authorized.discovering": "Discovering tools…",
  "connections.callback.authorized.body":
    "Tools will refresh automatically. {count} tools discovered so far.",
  "connections.callback.authorized.redirect": "Redirecting in {seconds}s.",
  "connections.callback.authorized.goNow": "Go to connection now →",
  "connections.callback.failed.title": "Connection failed",
  "connections.callback.failed.bodyDenied":
    "{name} did not return a token. Nothing was added.",
  "connections.callback.failed.bodyGeneric":
    "We couldn't complete the OAuth handshake. Nothing was added.",
  "connections.callback.failed.options": "Options",
  "connections.callback.failed.response": "Response",
  "connections.callback.failed.reference": "Reference id",
  "connections.callback.failed.back": "Back to catalog",
  "connections.callback.failed.retry": "Try again",
  "connections.callback.steps.redirected": "Redirected",
  "connections.callback.steps.awaiting": "Awaiting consent",
  "connections.callback.steps.discover": "Discover tools",
  "connections.confirm.removeTitle": "Remove this connection?",
  "connections.confirm.removeBody":
    "Agents that depend on it will lose access immediately.",
  "connections.confirm.disconnectTitle": "Disconnect credentials?",
  "connections.confirm.disconnectBody":
    "The server stays in your list but agents can't call it until you reconnect.",
  "connections.confirm.cancel": "Cancel",
  "connections.confirm.confirm": "Confirm",
} as const;

export type TranslationKey = keyof typeof en;
export type TranslationTable = Record<TranslationKey, string>;

export default en satisfies TranslationTable;
