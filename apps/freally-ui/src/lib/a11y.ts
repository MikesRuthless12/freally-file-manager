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
 * Stops propagation so a stacked overlay closes only the topmost one:
 * the innermost overlay mounts last, and its listener runs first
 * because `window` listeners fire in registration order.
 */
export function escapeToClose(node: HTMLElement, onClose: () => void) {
  let handler = onClose;

  function onKeydown(event: KeyboardEvent) {
    if (event.key !== "Escape") return;
    // Let a native control that owns Escape (an open <select> dropdown,
    // an IME composition) handle it first.
    if (event.defaultPrevented) return;
    event.stopPropagation();
    handler();
  }

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
    },
  };
}
