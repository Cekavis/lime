import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { AlertCircle, Check, Info, X } from "lucide-react";
import { getStatus, listModelPresets } from "@/api/commands";
import type { Config, ModelPreset, ServiceStatus } from "@/api/types";
import { Button } from "@/components/ui/button";
import { createRefreshQueue } from "@/lib/refresh-queue";
import { errorMessage } from "@/ui/format";
import { cn } from "@/lib/utils";

type Tone = "success" | "error" | "info";
type ActionResult<T> = { ok: true; value: T } | { ok: false };
interface ManagementContextValue {
  status: ServiceStatus | null;
  config: Config | null;
  presets: ModelPreset[];
  pending: string | null;
  connected: boolean;
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
  run: <T>(label: string, action: () => Promise<T>, success?: string) => Promise<ActionResult<T>>;
  notify: (message: string, tone?: Tone) => void;
}
const ManagementContext = createContext<ManagementContextValue | null>(null);

export function ManagementProvider({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<ServiceStatus | null>(null);
  const [presets, setPresets] = useState<ModelPreset[]>([]);
  const [pending, setPending] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ message: string; tone: Tone } | null>(null);
  const busy = useRef(false);
  const queue = useRef<ReturnType<typeof createRefreshQueue> | null>(null);
  const mounted = useRef(true);
  const notify = useCallback((message: string, tone: Tone = "info") => setNotice({ message, tone }), []);

  useEffect(() => {
    mounted.current = true;
    const refreshQueue = createRefreshQueue(async (isCurrent) => {
      if (busy.current) return;
      setLoading(true);
      const [nextStatus, nextPresets] = await Promise.allSettled([getStatus(), listModelPresets()]);
      if (!isCurrent()) return;
      if (nextStatus.status === "fulfilled") {
        setStatus(nextStatus.value);
        setError(nextPresets.status === "rejected" ? errorMessage(nextPresets.reason) : null);
      } else { setStatus(null); setError(errorMessage(nextStatus.reason)); }
      if (nextPresets.status === "fulfilled") setPresets(nextPresets.value);
      setLoading(false);
    });
    queue.current = refreshQueue;
    void refreshQueue.request();
    const timer = window.setInterval(() => { void refreshQueue.request(); }, 3000);
    const visible = () => { if (!document.hidden) void refreshQueue.request(); };
    document.addEventListener("visibilitychange", visible);
    return () => {
      mounted.current = false;
      refreshQueue.dispose();
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", visible);
    };
  }, []);

  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => setNotice(null), notice.tone === "error" ? 8000 : 4000);
    return () => window.clearTimeout(timer);
  }, [notice]);

  const refresh = useCallback(() => queue.current?.request() ?? Promise.resolve(), []);
  const run = useCallback(async <T,>(label: string, action: () => Promise<T>, success?: string): Promise<ActionResult<T>> => {
    if (busy.current) return { ok: false };
    busy.current = true;
    setPending(label);
    queue.current?.invalidate();
    try {
      const value = await action();
      if (mounted.current && success) notify(success, "success");
      return { ok: true, value };
    } catch (cause) {
      if (mounted.current) notify(errorMessage(cause), "error");
      return { ok: false };
    } finally {
      busy.current = false;
      queue.current?.invalidate();
      if (mounted.current) { setPending(null); void refresh(); }
    }
  }, [notify, refresh]);

  const NoticeIcon = notice?.tone === "error" ? AlertCircle : notice?.tone === "success" ? Check : Info;
  return <ManagementContext.Provider value={{ status, config: status?.config.config ?? null, presets, pending, connected: !!status && status.state !== "unavailable", loading, error, refresh, run, notify }}>
    {children}
    {notice && <div className="toast-frame" role={notice.tone === "error" ? "alert" : "status"}>
      <NoticeIcon aria-hidden className={cn("mt-0.5 size-4 shrink-0", notice.tone === "error" ? "text-destructive" : "text-primary")} />
      <span className="min-w-0 flex-1 break-words">{notice.message}</span>
      <Button variant="ghost" size="icon-sm" onClick={() => setNotice(null)} aria-label="关闭提示"><X /></Button>
    </div>}
  </ManagementContext.Provider>;
}

export function useManagement() {
  const value = useContext(ManagementContext);
  if (!value) throw new Error("ManagementProvider is missing");
  return value;
}
