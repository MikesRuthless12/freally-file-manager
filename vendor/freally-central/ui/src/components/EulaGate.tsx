import { useEffect, useRef, useState, type UIEvent } from "react";
import { exit } from "@tauri-apps/plugin-process";
import eulaText from "../legal/eula.md?raw";
import { useT } from "../i18n";
import { useFocusTrap } from "../panel";

// How close to the bottom (px) counts as "read to the end" — used both to enable
// Accept on scroll and to auto-enable it when the text is too short to scroll.
const SCROLL_END_SLACK = 12;

// First-run EULA gate (FC-06): the app does not proceed until the user accepts.
// Accept is enabled only after the text is scrolled to the end; acceptance is
// remembered by the caller (App.tsx). Decline closes the app.
export function EulaGate({ onAccept }: { onAccept: () => void }) {
  const t = useT();
  const [atEnd, setAtEnd] = useState(false);
  const dialogRef = useRef<HTMLDivElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  // Trap focus in the gate (no Escape — the user must Accept or Decline).
  useFocusTrap(dialogRef);

  // Move focus to the scrollable text so a keyboard-only user can PageDown/End
  // to the bottom and enable Accept. If the text is short enough that it doesn't
  // scroll, there's nothing to scroll to — enable Accept immediately. (Runs
  // after the focus trap's own focus-in, since it's declared later.)
  useEffect(() => {
    const el = bodyRef.current;
    if (!el) return;
    if (el.scrollHeight <= el.clientHeight + SCROLL_END_SLACK) setAtEnd(true);
    else el.focus();
  }, []);

  const onScroll = (e: UIEvent<HTMLDivElement>) => {
    const el = e.currentTarget;
    if (el.scrollTop + el.clientHeight >= el.scrollHeight - SCROLL_END_SLACK) setAtEnd(true);
  };

  const decline = () => {
    // You must accept to use the app; declining quits it.
    void exit(0).catch(() => {
      /* not running under Tauri (dev/test) — no-op */
    });
  };

  return (
    <div
      className="eula-overlay"
      role="dialog"
      aria-modal="true"
      aria-labelledby="eula-title"
      ref={dialogRef}
      tabIndex={-1}
    >
      <div className="eula">
        <h2 id="eula-title" className="eula-title">
          {t("eula-title")}
        </h2>
        <p className="eula-intro">{t("eula-intro")}</p>
        <div
          className="eula-body"
          onScroll={onScroll}
          ref={bodyRef}
          tabIndex={0}
          role="region"
          aria-label={t("eula-title")}
        >
          <pre className="eula-text">{eulaText}</pre>
        </div>
        {!atEnd && <p className="eula-hint">{t("eula-scroll-hint")}</p>}
        <div className="eula-actions">
          <button type="button" className="btn btn-ghost" onClick={decline}>
            {t("eula-decline")}
          </button>
          <button type="button" className="btn btn-primary" disabled={!atEnd} onClick={onAccept}>
            {t("eula-accept")}
          </button>
        </div>
      </div>
    </div>
  );
}
