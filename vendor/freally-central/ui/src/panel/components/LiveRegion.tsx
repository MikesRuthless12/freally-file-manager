interface LiveRegionProps {
  /** The text to announce. Toggle it between "" and a message; never feed it a
   *  rapidly-changing value (e.g. a download percent) — that belongs on the
   *  bar's role="progressbar" and would flood a screen reader here. */
  message: string;
}

// Always-mounted, visually hidden polite live region (FC-61). Screen readers
// reliably announce a change to a region's text only when the region already
// exists in the DOM — one inserted together with its text is often skipped — so
// this component stays mounted and callers change only `message`. Used for
// download/install/update outcomes and phase changes.
export function LiveRegion({ message }: LiveRegionProps) {
  return (
    <span className="fcp-sr-only" role="status">
      {message}
    </span>
  );
}
