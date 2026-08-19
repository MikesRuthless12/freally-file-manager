import { useState } from "react";
import { Modal } from "../panel";
import { useI18n, useT } from "../i18n";
import { localeName } from "../i18n/localeNames";
import type { Theme } from "../theme";
import { AboutDialog } from "./About";
import { WhatsNewDialog } from "./WhatsNew";
import { UpdatesDialog } from "./Updates";

type SubPanel = "about" | "whats-new" | "updates" | null;

interface SettingsDialogProps {
  onClose: () => void;
  theme: Theme;
  onToggleTheme: () => void;
}

export function SettingsDialog({ onClose, theme, onToggleTheme }: SettingsDialogProps) {
  const t = useT();
  const { locale, setLocale, locales } = useI18n();
  const [sub, setSub] = useState<SubPanel>(null);

  if (sub === "about") {
    return <AboutDialog onClose={() => setSub(null)} onCheckUpdates={() => setSub("updates")} />;
  }
  if (sub === "whats-new") return <WhatsNewDialog onClose={() => setSub(null)} />;
  if (sub === "updates") return <UpdatesDialog onClose={() => setSub(null)} />;

  return (
    <Modal title={t("settings-title")} closeLabel={t("modal-close")} onClose={onClose}>
      <div className="settings">
        <section className="settings-row">
          <span className="settings-label">{t("settings-theme")}</span>
          <button type="button" className="btn" onClick={onToggleTheme}>
            {theme === "dark" ? t("settings-theme-dark") : t("settings-theme-light")}
          </button>
        </section>

        <section className="settings-row">
          <label className="settings-label" htmlFor="settings-language">
            {t("language")}
          </label>
          <select
            id="settings-language"
            value={locale}
            onChange={(e) => setLocale(e.target.value)}
          >
            {locales.map((l) => (
              <option key={l} value={l}>
                {localeName(l)}
              </option>
            ))}
          </select>
        </section>

        <section className="settings-links">
          <button type="button" className="btn" onClick={() => setSub("updates")}>
            {t("settings-check-updates")}
          </button>
          <button type="button" className="btn" onClick={() => setSub("whats-new")}>
            {t("whats-new-title")}
          </button>
          <button type="button" className="btn" onClick={() => setSub("about")}>
            {t("about-title")}
          </button>
        </section>
      </div>
    </Modal>
  );
}
