// Locale-aware formatting for the live release data. Numbers use the active
// UI language's grouping; dates render as a short, localized calendar date.
// Intl formatters are comparatively expensive to construct, so they're cached
// per locale (these render in card grids and per-OS loops on every keystroke).

const numberFormatters = new Map<string, Intl.NumberFormat>();
function numberFormatter(locale: string): Intl.NumberFormat {
  let formatter = numberFormatters.get(locale);
  if (!formatter) {
    formatter = new Intl.NumberFormat(locale);
    numberFormatters.set(locale, formatter);
  }
  return formatter;
}

export function formatCount(locale: string, count: number): string {
  try {
    return numberFormatter(locale).format(count);
  } catch {
    return String(count);
  }
}

const percentFormatters = new Map<string, Intl.NumberFormat>();
function percentFormatter(locale: string): Intl.NumberFormat {
  let formatter = percentFormatters.get(locale);
  if (!formatter) {
    // FC-31's precise percent: always exactly two decimals ("38.92%").
    formatter = new Intl.NumberFormat(locale, {
      style: "percent",
      minimumFractionDigits: 2,
      maximumFractionDigits: 2,
    });
    percentFormatters.set(locale, formatter);
  }
  return formatter;
}

// Formats a download-progress fraction (0.3892 → "38.92%"). Callers pass real
// bytes-over-total fractions (see downloads/progress.ts) — already in [0, 1].
export function formatPercent(locale: string, value: number): string {
  try {
    return percentFormatter(locale).format(value);
  } catch {
    return `${(value * 100).toFixed(2)}%`;
  }
}

const dateFormatters = new Map<string, Intl.DateTimeFormat>();
function dateFormatter(locale: string): Intl.DateTimeFormat {
  let formatter = dateFormatters.get(locale);
  if (!formatter) {
    // Rendered in UTC so the calendar date matches GitHub's — a release published
    // just after midnight UTC must not read as the previous day in a western zone.
    formatter = new Intl.DateTimeFormat(locale, {
      year: "numeric",
      month: "short",
      day: "numeric",
      timeZone: "UTC",
    });
    dateFormatters.set(locale, formatter);
  }
  return formatter;
}

// Formats an ISO 8601 timestamp (GitHub `published_at`) as e.g. "Jul 2, 2026".
// Returns null for missing/invalid dates so callers can hide the field.
export function formatDate(locale: string, iso: string): string | null {
  if (!iso) return null;
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return null;
  try {
    return dateFormatter(locale).format(date);
  } catch {
    return iso;
  }
}
