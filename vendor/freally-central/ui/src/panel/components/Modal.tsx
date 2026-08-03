import { useRef, type ReactNode } from "react";
import { useFocusTrap } from "../useFocusTrap";

interface ModalProps {
  title: string;
  /** Localized label for the ✕ close button (accessibility name). */
  closeLabel: string;
  onClose: () => void;
  children: ReactNode;
  wide?: boolean;
}

// Shared modal shell (Central's equivalent of Capture's PickerShell): overlay,
// titled header with a close button, click-outside-to-close, plus the shared
// focus trap (FC-61) that moves focus in on open, keeps Tab within the dialog,
// closes on Escape, and restores focus on close. Purely presentational —
// callers pass the localized close label, so it works both inside the panel and
// in a host app's own dialogs.
export function Modal({ title, closeLabel, onClose, children, wide }: ModalProps) {
  const dialogRef = useRef<HTMLDivElement>(null);
  useFocusTrap(dialogRef, onClose);

  return (
    <div className="modal-overlay" role="presentation" onClick={onClose}>
      <div
        ref={dialogRef}
        className={wide ? "modal modal--wide" : "modal"}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-head">
          <h2 className="modal-title">{title}</h2>
          <button type="button" className="icon-btn" onClick={onClose} aria-label={closeLabel}>
            ✕
          </button>
        </div>
        <div className="modal-body">{children}</div>
      </div>
    </div>
  );
}
