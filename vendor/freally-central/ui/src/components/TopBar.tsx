import { useT } from "../i18n";
import type { Theme } from "../theme";

interface TopBarProps {
  theme: Theme;
  onToggleTheme: () => void;
  onOpenSettings: () => void;
}

export function TopBar({ theme, onToggleTheme, onOpenSettings }: TopBarProps) {
  const t = useT();
  return (
    <header className="topbar">
      <div className="brand">
        <img className="brand-logo" src="/logo.png" alt="" width={30} height={30} />
        <span className="brand-name">{t("app-name")}</span>
      </div>
      <div className="topbar-actions">
        <button
          type="button"
          className="icon-btn"
          onClick={onToggleTheme}
          aria-label={t("theme-toggle")}
          title={t("theme-toggle")}
        >
          {theme === "dark" ? "☀" : "☾"}
        </button>
        <button
          type="button"
          className="icon-btn"
          onClick={onOpenSettings}
          aria-label={t("settings-title")}
          title={t("settings-title")}
        >
          ⚙
        </button>
      </div>
    </header>
  );
}
