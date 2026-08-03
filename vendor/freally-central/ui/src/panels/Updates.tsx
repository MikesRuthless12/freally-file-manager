import { useCallback, useEffect, useState } from "react";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { LiveRegion, Modal } from "../panel";
import { useT } from "../i18n";

type Phase =
  | { kind: "checking" }
  | { kind: "uptodate" }
  | { kind: "available"; update: Update }
  | { kind: "downloading"; version: string; pct: number | null }
  | { kind: "installed" }
  | { kind: "error"; message: string };

// Self-hosted auto-updater: checks the signed latest.json on the GitHub releases
// endpoint (tauri.conf.json → plugins.updater). Every download is verified against
// the bundled minisign public key before it is applied — an unsigned or tampered
// package is refused by the plugin. Nothing downloads without an explicit click.
export function UpdatesDialog({ onClose }: { onClose: () => void }) {
  const t = useT();
  const [phase, setPhase] = useState<Phase>({ kind: "checking" });

  const runCheck = useCallback(async () => {
    setPhase({ kind: "checking" });
    try {
      const update = await check();
      setPhase(update ? { kind: "available", update } : { kind: "uptodate" });
    } catch (err) {
      setPhase({ kind: "error", message: String(err) });
    }
  }, []);

  useEffect(() => {
    let alive = true;
    check()
      .then((update) => {
        if (alive) setPhase(update ? { kind: "available", update } : { kind: "uptodate" });
      })
      .catch((err) => {
        if (alive) setPhase({ kind: "error", message: String(err) });
      });
    return () => {
      alive = false;
    };
  }, []);

  const install = useCallback(async (update: Update) => {
    let downloaded = 0;
    let total = 0;
    setPhase({ kind: "downloading", version: update.version, pct: null });
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          total = event.data.contentLength ?? 0;
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
          const pct = total > 0 ? Math.min(100, (downloaded / total) * 100) : null;
          setPhase({ kind: "downloading", version: update.version, pct });
        }
      });
      setPhase({ kind: "installed" });
    } catch (err) {
      setPhase({ kind: "error", message: String(err) });
    }
  }, []);

  // A short, stable label per phase for the live region below — deliberately NOT
  // the streaming percent, which changes many times a second and would flood a
  // screen reader. Errors are already announced by the visible role="alert".
  const liveStatus = (() => {
    switch (phase.kind) {
      case "checking":
        return t("updates-checking");
      case "uptodate":
        return t("updates-uptodate");
      case "available":
        return t("updates-available", { version: phase.update.version });
      case "downloading":
        return t("updates-downloading", { version: phase.version });
      case "installed":
        return t("updates-installed");
      default:
        return "";
    }
  })();

  return (
    <Modal title={t("updates-title")} closeLabel={t("modal-close")} onClose={onClose} wide>
      {/* Announces each phase change (checking → up to date / available /
          downloading / installed) — one announcement per transition. */}
      <LiveRegion message={liveStatus} />
      <div className="updates">
        {phase.kind === "checking" && <p className="muted">{t("updates-checking")}</p>}

        {phase.kind === "uptodate" && (
          <>
            <p className="ok">{t("updates-uptodate")}</p>
            <div className="row-btns">
              <button type="button" className="btn" onClick={runCheck}>
                {t("updates-check-again")}
              </button>
            </div>
          </>
        )}

        {phase.kind === "available" && (
          <>
            <p>
              {t("updates-available", { version: phase.update.version })}
              {phase.update.currentVersion ? (
                <span className="muted">
                  {" "}
                  {t("updates-current-version", { current: phase.update.currentVersion })}
                </span>
              ) : null}
            </p>
            {phase.update.body ? (
              <div className="notes-wrap">
                <label htmlFor="release-notes" className="notes-label">
                  {t("updates-release-notes-label", { version: phase.update.version })}
                </label>
                <textarea id="release-notes" readOnly value={phase.update.body} rows={10} className="notes-box" />
              </div>
            ) : null}
            <p className="muted">{t("updates-confirm")}</p>
            <div className="row-btns">
              <button type="button" className="btn btn-primary" onClick={() => install(phase.update)}>
                {t("updates-yes-update-now")}
              </button>
              <button type="button" className="btn" onClick={onClose}>
                {t("updates-no-not-now")}
              </button>
            </div>
          </>
        )}

        {phase.kind === "downloading" && (
          <div className="dl-progress">
            <div className="dl-progress-head">
              <span>{t("updates-downloading", { version: phase.version })}</span>
              <span className="mono">
                {phase.pct !== null ? `${phase.pct.toFixed(2)}%` : t("updates-starting")}
              </span>
            </div>
            <div
              className="bar"
              role="progressbar"
              aria-label={t("updates-downloading", { version: phase.version })}
              aria-valuemin={0}
              aria-valuemax={100}
              // Omit aria-valuenow while starting (pct null) so it reads as
              // indeterminate rather than stuck at 0%.
              {...(phase.pct !== null ? { "aria-valuenow": Number(phase.pct.toFixed(2)) } : {})}
            >
              <div
                className="bar-fill"
                style={{ width: phase.pct !== null ? `${phase.pct.toFixed(2)}%` : "8%" }}
              />
            </div>
          </div>
        )}

        {phase.kind === "installed" && (
          <>
            <p className="ok">{t("updates-installed")}</p>
            <div className="row-btns">
              <button type="button" className="btn btn-primary" onClick={() => void relaunch()}>
                {t("updates-restart-now")}
              </button>
              <button type="button" className="btn" onClick={onClose}>
                {t("updates-restart-later")}
              </button>
            </div>
          </>
        )}

        {phase.kind === "error" && (
          <>
            <p role="alert" className="err">
              {phase.message}
            </p>
            <div className="row-btns">
              <button type="button" className="btn" onClick={runCheck}>
                {t("updates-try-again")}
              </button>
            </div>
          </>
        )}
      </div>
    </Modal>
  );
}
