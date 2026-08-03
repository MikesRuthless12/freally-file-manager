// Public surface of the "Central inside" embeddable panel (FC-50).
// Hosts import only from here; see EMBEDDING.md for the drop-in steps.
export { CentralPanel, type CentralPanelProps } from "./CentralPanel";
export { Modal } from "./components/Modal";
export { LiveRegion } from "./components/LiveRegion";
export { useFocusTrap } from "./useFocusTrap";
export { isSafeHttpUrl, type PanelHost } from "./host";
export type { Translate, TranslateArgs } from "./i18n";
