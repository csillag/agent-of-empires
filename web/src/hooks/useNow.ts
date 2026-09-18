import { useEffect, useState } from "react";

/** Ticks a `Date.now()` snapshot on an interval, for render-safe "how long
 *  ago" labels. Reading `Date.now()` directly during render trips
 *  `react-hooks/purity`; this owns the one `setInterval` per caller instead
 *  of leaving each caller to hand-roll its own tick state. */
export function useNow(intervalMs: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(id);
  }, [intervalMs]);
  return now;
}
