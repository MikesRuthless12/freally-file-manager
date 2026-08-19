// Native display names for the language selector.
export const LOCALE_NAMES: Record<string, string> = {
  en: "English",
  ar: "العربية",
  de: "Deutsch",
  es: "Español",
  fr: "Français",
  hi: "हिन्दी",
  id: "Bahasa Indonesia",
  it: "Italiano",
  ja: "日本語",
  ko: "한국어",
  nl: "Nederlands",
  pl: "Polski",
  "pt-BR": "Português (BR)",
  ru: "Русский",
  tr: "Türkçe",
  uk: "Українська",
  vi: "Tiếng Việt",
  "zh-CN": "简体中文",
};

export function localeName(code: string): string {
  return LOCALE_NAMES[code] ?? code;
}
