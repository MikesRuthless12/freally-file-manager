/* eslint-disable react-refresh/only-export-components -- provider + hooks live together by design */
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { FluentBundle, FluentResource, type FluentVariable } from "@fluent/bundle";
import { LOCALE_NAMES } from "./localeNames";

// Eagerly load every locale catalog as raw text (Vite import.meta.glob):
// Central's own strings plus the embeddable panel's fcp-* catalogs — the same
// two-resource setup every host app embedding the panel uses.
const ftlModules = import.meta.glob("./locales/*.ftl", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;
const panelFtlModules = import.meta.glob("../panel/locales/*.ftl", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

function byLocale(modules: Record<string, string>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [path, content] of Object.entries(modules)) {
    const match = /\/([A-Za-z-]+)\.ftl$/.exec(path);
    if (match) out[match[1]] = content;
  }
  return out;
}

const resources = byLocale(ftlModules);
const panelResources = byLocale(panelFtlModules);

// Language-picker order: English first, then the rest alphabetically by their
// NATIVE name. A fixed "en" collator (not the active locale) keeps the order
// stable no matter which UI language is selected.
function orderLocales(codes: string[]): string[] {
  const collator = new Intl.Collator("en", { sensitivity: "base" });
  const rest = codes
    .filter((code) => code !== "en")
    .sort((a, b) => collator.compare(LOCALE_NAMES[a] ?? a, LOCALE_NAMES[b] ?? b));
  return codes.includes("en") ? ["en", ...rest] : rest;
}

export const LOCALES = orderLocales(Object.keys(resources));
const DEFAULT_LOCALE = "en";
const STORAGE_KEY = "fc.locale";
const RTL_LOCALES = new Set(["ar"]);

const bundleCache = new Map<string, FluentBundle>();
function getBundle(locale: string): FluentBundle {
  let bundle = bundleCache.get(locale);
  if (!bundle) {
    // useIsolating adds Unicode bidi marks around placeables; off for cleaner UI strings.
    bundle = new FluentBundle(locale, { useIsolating: false });
    bundle.addResource(new FluentResource(resources[locale] ?? ""));
    bundle.addResource(new FluentResource(panelResources[locale] ?? ""));
    bundleCache.set(locale, bundle);
  }
  return bundle;
}

export type TranslateArgs = Record<string, FluentVariable>;
export type Translate = (id: string, args?: TranslateArgs) => string;

function translate(locale: string, id: string, args?: TranslateArgs): string {
  for (const loc of locale === DEFAULT_LOCALE ? [locale] : [locale, DEFAULT_LOCALE]) {
    const bundle = getBundle(loc);
    const message = bundle.getMessage(id);
    if (message?.value) {
      return bundle.formatPattern(message.value, args);
    }
  }
  return id; // last-resort: show the key (i18n:lint prevents this shipping)
}

function detectInitialLocale(): string {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved && resources[saved]) return saved;
  } catch {
    /* localStorage unavailable */
  }
  const preferred =
    typeof navigator !== "undefined"
      ? (navigator.languages ?? [navigator.language]).filter(Boolean)
      : [];
  for (const lang of preferred) {
    if (resources[lang]) return lang;
    const base = lang.split("-")[0];
    const match = LOCALES.find((l) => l === base || l.split("-")[0] === base);
    if (match) return match;
  }
  return DEFAULT_LOCALE;
}

interface I18n {
  locale: string;
  setLocale: (locale: string) => void;
  locales: string[];
  t: Translate;
}

const I18nContext = createContext<I18n | null>(null);

export function I18nProvider({ children }: { children: ReactNode }) {
  const [locale, setLocaleState] = useState<string>(detectInitialLocale);

  const setLocale = useCallback((next: string) => {
    setLocaleState(next);
    try {
      localStorage.setItem(STORAGE_KEY, next);
    } catch {
      /* ignore */
    }
  }, []);

  useEffect(() => {
    document.documentElement.lang = locale;
    document.documentElement.dir = RTL_LOCALES.has(locale) ? "rtl" : "ltr";
  }, [locale]);

  const t = useCallback<Translate>((id, args) => translate(locale, id, args), [locale]);

  const value = useMemo<I18n>(
    () => ({ locale, setLocale, locales: LOCALES, t }),
    [locale, setLocale, t],
  );

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18n {
  const ctx = useContext(I18nContext);
  if (!ctx) throw new Error("useI18n must be used within an I18nProvider");
  return ctx;
}

export function useT(): Translate {
  return useI18n().t;
}
