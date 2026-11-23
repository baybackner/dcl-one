// A buy dialog's page reads its data again once a purchase went out, but only when the dialog is gone: a read empties
// the page until it answers, and would take the dialog and its notice with it. The dialog's outcome arms it; its close
// fires it, and so does the page unmounting first (browser Back on the notice), so the refresh is never lost.

import { useCallback, useEffect, useRef } from "react";

export function useReadOnClose(read: () => void): { arm(): void; fire(): void } {
  const armed = useRef(false);
  const latest = useRef(read);
  latest.current = read;
  const arm = useCallback(() => {
    armed.current = true;
  }, []);
  const fire = useCallback(() => {
    if (!armed.current) return;
    armed.current = false;
    latest.current();
  }, []);
  useEffect(() => fire, [fire]);
  return { arm, fire };
}
