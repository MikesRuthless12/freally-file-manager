import { useRef } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useFocusTrap } from "./useFocusTrap";

// A minimal dialog that wires the trap the way Modal/EulaGate do: a tabIndex=-1
// container the hook can focus, with two tabbable controls inside.
function Dialog({ onEscape }: { onEscape?: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  useFocusTrap(ref, onEscape);
  return (
    <div ref={ref} tabIndex={-1} data-testid="dialog">
      <button data-testid="first">first</button>
      <button data-testid="last">last</button>
    </div>
  );
}

afterEach(cleanup);

describe("useFocusTrap", () => {
  it("moves focus into the container on open", () => {
    render(<Dialog />);
    expect((document.activeElement as HTMLElement).dataset.testid).toBe("dialog");
  });

  it("restores focus to the launching element on close", () => {
    const trigger = document.createElement("button");
    document.body.appendChild(trigger);
    trigger.focus();
    expect(document.activeElement).toBe(trigger);

    const { unmount } = render(<Dialog />);
    // Focus moved off the trigger and into the dialog while open…
    expect(document.activeElement).not.toBe(trigger);
    unmount();
    // …and returns to the trigger once the dialog closes.
    expect(document.activeElement).toBe(trigger);
    trigger.remove();
  });

  it("closes on Escape when onEscape is given", () => {
    const onEscape = vi.fn();
    render(<Dialog onEscape={onEscape} />);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onEscape).toHaveBeenCalledTimes(1);
  });

  it("ignores Escape when no onEscape is given (e.g. the EULA gate)", () => {
    // No handler, so a stray Escape must not throw or close anything.
    render(<Dialog />);
    expect(() => fireEvent.keyDown(document, { key: "Escape" })).not.toThrow();
  });

  it("wraps Tab from the last control back to the first", () => {
    render(<Dialog />);
    screen.getByTestId("last").focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(document.activeElement).toBe(screen.getByTestId("first"));
  });

  it("wraps Shift+Tab from the first control to the last", () => {
    render(<Dialog />);
    screen.getByTestId("first").focus();
    fireEvent.keyDown(document, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(screen.getByTestId("last"));
  });
});
