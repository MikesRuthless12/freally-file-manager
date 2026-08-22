/**
 * Callout annotations for the documentation screenshots.
 *
 * `docs/documentation.html` walks the reader through each flow step by
 * step; these helpers put a red box around the control a step is talking
 * about and bake it into the PNG, so the picture answers "which thing do
 * I click" on its own.
 *
 * The box is measured from the element's own bounding box at capture
 * time rather than hand-placed, so it stays correct when the layout
 * changes — a hand-placed overlay silently drifts off its target and
 * nothing fails.
 *
 * Overlays are `position: fixed` in viewport coordinates, which is what
 * `boundingBox()` returns, and are removed after every shot so they
 * cannot leak into the next one.
 */

import type { Locator, Page } from "@playwright/test";

/** Breathing room between the control's edge and the box, in CSS px. */
const PAD = 6;

/** One annotated target: the element, and the step number to badge it with. */
export type Callout = { target: Locator; step?: number };

const CLASS = "__freally_docs_callout__";

/**
 * Draw a red box (and optional numbered badge) over each target.
 *
 * Scrolls each target into view first — `boundingBox()` on an element
 * outside the viewport returns coordinates the overlay would render off
 * screen, which produces a screenshot with no visible box and no error.
 */
export async function drawCallouts(page: Page, callouts: Callout[]): Promise<void> {
  for (const { target, step } of callouts) {
    await target.scrollIntoViewIfNeeded();
    const rect = await target.boundingBox();
    if (!rect) {
      throw new Error("callout target has no bounding box (not rendered?)");
    }
    await page.evaluate(
      ({ rect, step, cls, pad }) => {
        // Clamp to the viewport. Controls that sit against an edge — the
        // whole footer strip does — otherwise get a box whose outer half
        // is drawn off screen and simply does not appear in the capture.
        const x0 = Math.max(1, rect.x - pad);
        const y0 = Math.max(1, rect.y - pad);
        const x1 = Math.min(window.innerWidth - 1, rect.x + rect.width + pad);
        const y1 = Math.min(window.innerHeight - 1, rect.y + rect.height + pad);

        const box = document.createElement("div");
        box.className = cls;
        Object.assign(box.style, {
          position: "fixed",
          left: `${x0}px`,
          top: `${y0}px`,
          width: `${x1 - x0}px`,
          height: `${y1 - y0}px`,
          border: "3px solid #e5484d",
          borderRadius: "10px",
          boxShadow: "0 0 0 3px rgba(229,72,77,0.22), 0 2px 12px rgba(0,0,0,0.35)",
          pointerEvents: "none",
          zIndex: "2147483647",
        } as Partial<CSSStyleDeclaration>);
        document.body.appendChild(box);

        if (typeof step === "number") {
          const badge = document.createElement("div");
          badge.className = cls;
          badge.textContent = String(step);
          Object.assign(badge.style, {
            position: "fixed",
            // Sit on the box's top-left corner, half in / half out —
            // nudged inward when that corner is against a viewport edge.
            left: `${Math.max(2, x0 - 13)}px`,
            top: `${Math.max(2, y0 - 13)}px`,
            width: "26px",
            height: "26px",
            borderRadius: "50%",
            background: "#e5484d",
            color: "#fff",
            font: "700 15px/26px system-ui, -apple-system, Segoe UI, sans-serif",
            textAlign: "center",
            pointerEvents: "none",
            zIndex: "2147483647",
            boxShadow: "0 2px 8px rgba(0,0,0,0.4)",
          } as Partial<CSSStyleDeclaration>);
          document.body.appendChild(badge);
        }
      },
      { rect, step, cls: CLASS, pad: PAD },
    );
  }
}

/** Remove every overlay this module added. */
export async function clearCallouts(page: Page): Promise<void> {
  await page.evaluate((cls) => {
    for (const el of Array.from(document.querySelectorAll(`.${cls}`))) el.remove();
  }, CLASS);
}
