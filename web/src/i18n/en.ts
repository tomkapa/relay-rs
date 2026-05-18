// English string table.
//
// Keep keys flat and namespaced (`area.subarea.role`). The translation
// values are render-only — no formatting placeholders today; if/when one
// grows them, switch to a tiny `format(t('x'), {…})` helper rather than
// reaching for a runtime dep prematurely (CLAUDE.md §8 spirit).
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
} as const;

export type TranslationKey = keyof typeof en;
export type TranslationTable = Record<TranslationKey, string>;

export default en satisfies TranslationTable;
