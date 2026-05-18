// In-tree i18n shim.
//
// Why not a runtime dep (i18next / react-i18next): the surface is four
// components and one language switcher. Reaching for a full library
// (~30KB minified + ICU plumbing) for "lookup string in table, re-render
// on swap" trips the §8 "zero-dep bias" cost test in CLAUDE.md. The
// `t()` API mirrors react-i18next's `useTranslation().t` shape so a
// future swap is mechanical.
//
// Source of truth for the active language:
//   1. The active org's `default_language` in `useAuthStore` (post-auth).
//   2. The browser's `navigator.language` primary tag (pre-auth, for the
//      sign-in page).
//   3. `"en"` (final fallback).
//
// Post-auth, `useLangFromOrg()` subscribes to (1) and pushes the value
// into this module so `t()` returns the right string on every render.

import { useEffect } from "react";

import type { Language } from "../types/api";
import { useAuthStore } from "../stores/authStore";
import en, { type TranslationKey, type TranslationTable } from "./en";
import vi from "./vi";

const tables: Record<Language, TranslationTable> = { en, vi };

// Compile-time check that the two tables expose the same keys. A missing
// vi key is a TypeScript error at the `vi.ts` `TranslationTable` cast;
// this asserts the inverse at boot — an extra vi key (TS can't see it
// today since `Record<K, V>` is open) would surface here.
const enKeys = new Set(Object.keys(en) as TranslationKey[]);
for (const k of Object.keys(vi) as TranslationKey[]) {
  if (!enKeys.has(k)) {
    // Throwing on boot prevents the app from rendering with a partial
    // translation set — mirrors §6 "fail loud at the boundary".
    throw new Error(`i18n: vi.ts has key "${k}" not present in en.ts`);
  }
}

let currentLanguage: Language = detectInitialLanguage();

function detectInitialLanguage(): Language {
  if (typeof navigator === "undefined") return "en";
  const tag = navigator.language?.split(/[-_]/)[0]?.toLowerCase();
  return tag === "vi" ? "vi" : "en";
}

const listeners = new Set<() => void>();

/** Set the active language and notify subscribers. Idempotent on no-op. */
export function setLanguage(lang: Language): void {
  if (lang === currentLanguage) return;
  currentLanguage = lang;
  for (const l of listeners) l();
}

/** Current active language. Reads only — use `setLanguage` to mutate. */
export function getLanguage(): Language {
  return currentLanguage;
}

/** Translate a key. Falls back to the English value on a miss (every
 *  table has the same keys by construction, so a miss means a typo).
 *  When `vars` is supplied, `{name}`-style placeholders inside the
 *  template are replaced with the matching value. Unknown placeholders
 *  are left in place so a typo doesn't silently swallow content. */
export function t(
  key: TranslationKey,
  vars?: Record<string, string | number>,
): string {
  const raw = tables[currentLanguage][key] ?? en[key];
  if (!vars) return raw;
  return raw.replace(/\{(\w+)\}/g, (m, name: string) =>
    name in vars ? String(vars[name]) : m,
  );
}

/** React hook: returns a `t` bound to the current language that
 *  re-renders the calling component on every `setLanguage` call. The
 *  hook deliberately returns an object so a future swap to
 *  `react-i18next` is a one-line `const { t } = useTranslation()`. */
export function useT(): {
  t: (key: TranslationKey, vars?: Record<string, string | number>) => string;
  language: Language;
} {
  // Bare counter forces a re-render on every notification; no need to
  // store the actual language because every render reads through `t()`.
  const subscribe = (cb: () => void) => {
    listeners.add(cb);
    return () => {
      listeners.delete(cb);
    };
  };
  const getSnapshot = () => currentLanguage;
  // Inline `useSyncExternalStore` rather than importing — keeps the hook
  // self-contained and identical to what react-i18next would do.
  const language = useSyncExternalStoreImport(subscribe, getSnapshot);
  return { t, language };
}

// Lazy import to avoid a top-level circular when this file is the first
// touched (the React import path drags in JSX runtime which expects the
// rest of the app to be initialized).
import { useSyncExternalStore as useSyncExternalStoreImport } from "react";

/** Subscribe to the active org's `default_language` in `authStore` and
 *  push it into the i18n module. Mount once at the App root. On
 *  logout (or pre-auth) the language resets to the browser-detected
 *  default so the sign-in page doesn't keep showing the previous
 *  org's language after sign-out. */
export function useLangFromOrg(): void {
  const lang = useAuthStore((s) => {
    const me = s.me;
    if (!me) return null;
    const active = me.orgs.find((o) => o.id === me.active_org_id);
    return active?.default_language ?? null;
  });
  useEffect(() => {
    setLanguage(lang ?? detectInitialLanguage());
  }, [lang]);
}
