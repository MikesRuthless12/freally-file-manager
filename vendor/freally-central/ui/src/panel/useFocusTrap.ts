import { useEffect, useRef, type RefObject } from "react";

// Elements that can receive keyboard focus, for the Tab-wrap trap below.
const FOCUSABLE =
  'a[href], button:not([disabled]), textarea:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])';

// Tabbable descendants of `root`, minus ones an author has hidden from AT or
// removed from layout via the `hidden` attribute (a display:none subtree). We
// filter on attributes, not computed layout, so this is correct under jsdom too
// (which does no layout) — the panel never renders CSS-only-hidden focusables.
function tabbable(root: HTMLElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
    (el) => el.getAttribute("aria-hidden") !== "true" && el.closest("[hidden]") === null,
  );
}

/**
 * Accessibility for any modal/gate (FC-61). While the dialog is open this:
 *  - moves focus INTO `containerRef` on open, so keyboard and screen-reader
 *    users start inside the dialog rather than on the now-inert trigger behind
 *    the overlay (the container should carry `tabIndex={-1}`);
 *  - keeps Tab / Shift+Tab focus TRAPPED within the dialog — including pulling
 *    focus back in if it has somehow landed outside;
 *  - RESTORES focus to whatever was focused before, on close.
 *
 * When `onEscape` is given, Escape closes the dialog. It is claimed in the
 * capture phase and honours `defaultPrevented`, so a host shell embedding the
 * panel doesn't double-handle the key (see EMBEDDING.md).
 */
export function useFocusTrap(containerRef: RefObject<HTMLElement | null>, onEscape?: () => void) {
  // The latest onEscape without re-subscribing the listener on every render:
  // callers (Modal) pass a fresh onClose closure each render, and re-adding a
  // capture-phase document listener on every keystroke-adjacent render is pure
  // churn. Read it through a ref that a cheap effect keeps current.
  const onEscapeRef = useRef(onEscape);
  useEffect(() => {
    onEscapeRef.current = onEscape;
  }, [onEscape]);

  // Focus in on open, restore on close. Empty deps so the launching element is
  // captured exactly once — never a dialog node from a later re-render, which
  // would leave focus stranded on a detached element after close.
  useEffect(() => {
    const previouslyFocused = document.activeElement as HTMLElement | null;
    containerRef.current?.focus();
    return () => previouslyFocused?.focus?.();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const onEsc = onEscapeRef.current;
      if (onEsc && e.key === "Escape" && !e.defaultPrevented) {
        e.preventDefault();
        e.stopPropagation();
        onEsc();
        return;
      }
      if (e.key !== "Tab") return;
      const root = containerRef.current;
      if (!root) return;
      const items = tabbable(root);
      if (items.length === 0) {
        // Nothing tabbable inside — keep focus on the dialog itself.
        e.preventDefault();
        root.focus();
        return;
      }
      const first = items[0];
      const last = items[items.length - 1];
      const active = document.activeElement as HTMLElement | null;
      // Focus is outside the dialog entirely (e.g. it never landed, or a click
      // moved it to a background control): pull it back in rather than letting
      // Tab walk into the inert background.
      if (!active || !root.contains(active)) {
        e.preventDefault();
        (e.shiftKey ? last : first).focus();
        return;
      }
      if (e.shiftKey && (active === first || active === root)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [containerRef]);
}
