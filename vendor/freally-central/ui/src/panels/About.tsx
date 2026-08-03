import { useEffect, useState, type ReactNode } from "react";
import { buildInfo, openExternal } from "../api/commands";
import type { BuildInfo } from "../api/types";
import { Modal } from "../panel";
import { useT } from "../i18n";

interface AboutDialogProps {
  onClose: () => void;
  onCheckUpdates: () => void;
}

// About — everything derivable from the build comes from the build (version,
// authors, repository), so this panel can never claim a version the binary
// isn't. Links open with the OS handler.
export function AboutDialog({ onClose, onCheckUpdates }: AboutDialogProps) {
  const t = useT();
  const [info, setInfo] = useState<BuildInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    buildInfo()
      .then((next) => {
        if (alive) setInfo(next);
      })
      .catch((err) => {
        if (alive) setError(String(err));
      });
    return () => {
      alive = false;
    };
  }, []);

  return (
    <Modal title={t("about-title")} closeLabel={t("modal-close")} onClose={onClose} wide>
      <div className="about">
        <div className="about-brand">
          <span className="about-name">{t("app-name")}</span>
          <span className="about-tagline">{t("app-tagline")}</span>
        </div>

        {error && (
          <p role="alert" className="about-error">
            {error}
          </p>
        )}

        {info && (
          <dl className="about-grid">
            <Row label={t("about-version")} value={info.version} />
            <Row label={t("about-created-by")} value={info.authors} />
            <Row label={t("about-project-started")} value={info.projectStarted} />
            <Row
              label={t("about-first-stable")}
              value={info.firstStableReleased ?? t("about-first-stable-pending")}
            />
            <Row label={t("about-platform")} value={`${info.os} / ${info.arch}`} />
          </dl>
        )}

        <p className="about-note">{t("about-local-first")}</p>

        {info && (
          <div className="about-links">
            <LinkBtn href={info.homepage}>{t("about-website")}</LinkBtn>
            <LinkBtn href={info.issues}>{t("about-issues")}</LinkBtn>
            <LinkBtn href={`${info.repository}/blob/main/LICENSE`}>{t("about-license")}</LinkBtn>
            <LinkBtn href={`${info.repository}/blob/main/EULA.md`}>{t("about-eula")}</LinkBtn>
          </div>
        )}

        <div className="about-foot">
          <button type="button" className="btn btn-primary" onClick={onCheckUpdates}>
            {t("settings-check-updates")}
          </button>
          {info && <span className="about-copyright">{info.copyright}</span>}
        </div>
      </div>
    </Modal>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt>{label}</dt>
      <dd>{value}</dd>
    </>
  );
}

function LinkBtn({ href, children }: { href: string; children: ReactNode }) {
  return (
    <button type="button" className="link-btn" onClick={() => void openExternal(href)}>
      {children}
    </button>
  );
}
