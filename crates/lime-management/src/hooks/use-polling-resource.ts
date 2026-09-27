import { useCallback, useEffect, useRef, useState } from "react";
import { createRefreshQueue } from "@/lib/refresh-queue";
import { errorMessage } from "@/ui/format";

export function usePollingResource<T>({ load, active, key = "", intervalMs = 3000 }: {
  load: () => Promise<T>; active: boolean; key?: string; intervalMs?: number;
}) {
  const [data, setData] = useState<T>();
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const loadRef = useRef(load);
  loadRef.current = load;
  const queueRef = useRef<ReturnType<typeof createRefreshQueue> | null>(null);

  useEffect(() => {
    const queue = createRefreshQueue(async (isCurrent) => {
      setLoading(true);
      try {
        const value = await loadRef.current();
        if (isCurrent()) { setData(value); setError(null); }
      } catch (cause) {
        if (isCurrent()) setError(errorMessage(cause));
      } finally {
        if (isCurrent()) setLoading(false);
      }
    });
    queueRef.current = queue;
    setError(null);
    setLoading(false);
    return () => { queue.dispose(); };
  }, []);

  const refresh = useCallback(() => queueRef.current?.request() ?? Promise.resolve(), []);
  const invalidate = useCallback(() => queueRef.current?.invalidate(), []);

  useEffect(() => {
    invalidate();
    setError(null);
    setLoading(false);
    // Reuse the same queue when changing pages: the older request must settle
    // before the next one starts, and its response cannot be applied.
    if (active) void refresh();
  }, [key, invalidate, refresh]);

  useEffect(() => {
    if (!active) { invalidate(); setLoading(false); return; }
    void refresh();
    const timer = window.setInterval(() => { void refresh(); }, intervalMs);
    const visible = () => { if (!document.hidden) void refresh(); };
    document.addEventListener("visibilitychange", visible);
    return () => { invalidate(); window.clearInterval(timer); document.removeEventListener("visibilitychange", visible); };
  }, [active, intervalMs, refresh, invalidate]);
  return { data, error, loading, refresh, invalidate, setData };
}
