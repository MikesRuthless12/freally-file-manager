import { useState } from "react";
import { openExternal, revealInFolder } from "../api/commands";
import { useI18n } from "../i18n";
import { CentralPanel, type PanelHost } from "../panel";
import { SettingsDialog } from "../panels/Settings";
import { useTheme } from "../theme";
import { TopBar } from "./TopBar";

// The panel's shell actions, backed by Central's opener plugin. Module-level
// so the panel's `host` prop stays referentially stable across renders.
const HOST: PanelHost = { openExternal, revealInFolder };

// Central's own shell around the shared panel (FC-50): top bar, settings,
// footer. The grid/detail/download/install flow is the same <CentralPanel>
// every host app embeds — Central holds no copy of that logic.
export function Hub() {
  const { t, locale } = useI18n();
  const { theme, toggle } = useTheme();
  const [settingsOpen, setSettingsOpen] = useState(false);

  return (
    <div className="app">
      <TopBar theme={theme} onToggleTheme={toggle} onOpenSettings={() => setSettingsOpen(true)} />

      <main className="main">
        <CentralPanel t={t} locale={locale} host={HOST} />
      </main>

      <footer className="footer">{t("footer-note")}</footer>

      {settingsOpen && (
        <SettingsDialog onClose={() => setSettingsOpen(false)} theme={theme} onToggleTheme={toggle} />
      )}
    </div>
  );
}
