// Turning a rejected `invoke(...)` into text a human should read.
//
// The Rust command layer rejects with **Fluent keys**, not sentences —
// `err-schedule-id-invalid`, `err-favorite-kind-invalid`,
// `err-affinity-name-duplicate`. A bare `String(e)` therefore renders
// the key itself into the UI, in every locale. `t()` already falls back
// to `{key}` for anything it doesn't recognise, so routing through it
// costs nothing for messages that were never keys.

import { t } from "./i18n";

/**
 * Best-effort human text for a rejected IPC call.
 *
 * Handles the three shapes the backend actually produces:
 * - a bare Fluent key (`"err-schedule-id-invalid"`),
 * - a key with detail appended (`"err-schedule-install-failed: …"`),
 *   where the key is translated and the detail kept, and
 * - anything else (an `Error`, a plain string), passed through.
 */
export function errorText(e: unknown): string {
  const raw = e instanceof Error ? e.message : String(e);

  // `err-foo-bar: some detail` — translate the key, keep the detail.
  // The OS's own message is the useful half of an install failure, so
  // dropping it in favour of a generic string would be a downgrade.
  const withDetail = /^([a-z][a-z0-9-]*): ([\s\S]+)$/.exec(raw);
  if (withDetail) {
    const translated = t(withDetail[1]);
    if (translated !== `{${withDetail[1]}}`) {
      return `${translated} — ${withDetail[2]}`;
    }
    return raw;
  }

  if (/^[a-z][a-z0-9-]*$/.test(raw)) {
    const translated = t(raw);
    if (translated !== `{${raw}}`) return translated;
  }
  return raw;
}
