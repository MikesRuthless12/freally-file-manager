// FFM-M22 — accessibility helpers shared across overlays.
//
// The pre-existing pattern for "Escape closes this" was an `onkeydown`
// on the dialog element plus `tabindex={-1}`. That works only while
// focus is inside the dialog — press Tab past the last control, or
// click a control that moves focus to `document.body`, and Escape stops
// working with no visible reason. Several overlays never had it at all.
//
// `escapeToClose` listens on `window` instead, so the key works from
// anywhere while the overlay is mounted. It deliberately does **not**
// trap focus: the stable-gate requirement is that no overlay can trap
// the keyboard, and a home-grown trap is a far more likely source of
// one than its absence.

/**
 * Svelte action: call `onClose` when Escape is pressed anywhere while
 * this element is mounted.
 *
 * ```svelte
 * <div class="drawer" use:escapeToClose={close}>…</div>
 * ```
 *
 * With overlays stacked (RestoreModal over LibraryDrawer), only the
 * topmost one closes. That is enforced by the `openOverlays` stack
 * below, not by `stopPropagation`: every instance listens on `window`,
 * and `stopPropagation` does not suppress other listeners on the *same*
 * target, so one Escape would otherwise fire all of them at once. The
 * registration order runs the wrong way too — the innermost overlay
 * mounts last, so its listener runs last, not first.
 */

/**
 * Mount order of the overlays currently listening for Escape. The last
 * entry is the topmost, and only it acts on the key.
 */
const openOverlays: HTMLElement[] = [];

export function escapeToClose(node: HTMLElement, onClose: () => void) {
  let handler = onClose;

  function onKeydown(event: KeyboardEvent) {
    if (event.key !== "Escape") return;
    // Let a native control that owns Escape (an open <select> dropdown,
    // an IME composition) handle it first.
    if (event.defaultPrevented) return;
    // Only the topmost overlay responds; the ones beneath stay open.
    if (openOverlays[openOverlays.length - 1] !== node) return;
    event.stopPropagation();
    handler();
  }

  openOverlays.push(node);
  window.addEventListener("keydown", onKeydown);
  // Mark the element so the e2e suite can assert an overlay opted in
  // rather than having to synthesise a key press per overlay.
  node.setAttribute("data-escape-closes", "true");

  return {
    update(next: () => void) {
      handler = next;
    },
    destroy() {
      window.removeEventListener("keydown", onKeydown);
      const i = openOverlays.lastIndexOf(node);
      if (i >= 0) openOverlays.splice(i, 1);
    },
  };
}
