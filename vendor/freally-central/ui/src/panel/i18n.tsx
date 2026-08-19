/* eslint-disable react-refresh/only-export-components -- provider + hooks live together by design */
// The panel's i18n boundary (FC-50): the panel never owns a Fluent runtime —
// the HOST app passes its own `t` and active locale in, so panel chrome is
// localized through the host's catalogs. The panel ships its `fcp-*` message
// set (ui/src/panel/locales/*.ftl) for hosts to load into their bundles.
import { createContext, useContext, useMemo, type ReactNode } from "react";

/** Argument shape every Freally host's Fluent `t` accepts. */
export type TranslateArgs = Record<string, string | number>;
export type Translate = (id: string, args?: TranslateArgs) => string;

interface PanelI18n {
  locale: string;
  t: Translate;
}

const PanelI18nContext = createContext<PanelI18n | null>(null);

export function PanelI18nProvider({
  t,
  locale,
  children,
}: PanelI18n & { children: ReactNode }) {
  const value = useMemo<PanelI18n>(() => ({ t, locale }), [t, locale]);
  return <PanelI18nContext.Provider value={value}>{children}</PanelI18nContext.Provider>;
}

export function useI18n(): PanelI18n {
  const ctx = useContext(PanelI18nContext);
  if (!ctx) throw new Error("Central panel components must render inside <CentralPanel>");
  return ctx;
}

export function useT(): Translate {
  return useI18n().t;
}
