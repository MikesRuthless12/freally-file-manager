import { useEffect, useState } from "react";
import { releaseNotes } from "../api/commands";
import type { ReleaseNotes } from "../api/types";
import { Modal } from "../panel";
import { useT } from "../i18n";

// Settings → What's New: the running build's changelog section, shown in-app.
// The notes are embedded at build time (release_notes reads CHANGELOG.md), so
// this is offline and always matches exactly what shipped.
export function WhatsNewDialog({ onClose }: { onClose: () => void }) {
  const t = useT();
  const [notes, setNotes] = useState<ReleaseNotes | null>(null);

  useEffect(() => {
    let alive = true;
    releaseNotes()
      .then((result) => {
        if (alive) setNotes(result);
      })
      .catch(() => {
        if (alive) setNotes({ version: "", notes: null });
      });
    return () => {
      alive = false;
    };
  }, []);

  return (
    <Modal title={t("whats-new-title")} closeLabel={t("modal-close")} onClose={onClose} wide>
      <div className="whatsnew">
        {notes === null ? (
          <p className="muted">{t("whats-new-loading")}</p>
        ) : notes.notes ? (
          <>
            <p className="muted">{t("whats-new-version", { version: notes.version })}</p>
            <textarea readOnly value={notes.notes} rows={16} className="notes-box" />
          </>
        ) : (
          <p className="muted">{t("whats-new-empty")}</p>
        )}
      </div>
    </Modal>
  );
}
