import { useEffect, useState } from "react";
import { Activity, BookOpen, FlaskConical, History, LoaderCircle, Monitor, Moon, RefreshCw, Settings2, Sun } from "lucide-react";
import { useManagement } from "./management-context";
import { useTheme } from "./theme-context";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { SettingsPage } from "@/pages/settings-page";
import { TestPage } from "@/pages/test-page";
import { BenchmarkPage } from "@/pages/benchmark-page";
import { DictionaryPage } from "@/pages/dictionary-page";
import { HistoryPage } from "@/pages/history-page";

const pages = [
  { id: "settings", label: "设置", icon: Settings2 },
  { id: "test", label: "测试", icon: FlaskConical },
  { id: "benchmark", label: "评测", icon: Activity },
  { id: "dictionary", label: "词库", icon: BookOpen },
  { id: "history", label: "历史", icon: History },
] as const;
type Page = typeof pages[number]["id"];
const stateLabels = { ready: "服务已连接", rime_only: "基础模式", reloading: "模型重载中", unavailable: "服务未连接" };

export function App() {
  const [page, setPage] = useState<Page>("settings");
  const { status, pending, loading, error, refresh } = useManagement();
  const { theme, setTheme } = useTheme();
  const state = status?.state ?? "unavailable";
  const ThemeIcon = theme === "system" ? Monitor : theme === "light" ? Sun : Moon;
  useEffect(() => { window.scrollTo({ top: 0 }); void refresh(); }, [page, refresh]);
  const navigation = <nav aria-label="主要导航" className="grid grid-cols-5 gap-1 md:flex md:flex-col md:gap-1.5">
    {pages.map(({ id, label, icon: Icon }) => <Button key={id} variant="ghost" aria-current={page === id ? "page" : undefined} aria-controls={`page-${id}`}
      className={cn("h-12 flex-col gap-1 rounded-lg px-1 text-xs text-sidebar-foreground md:h-10 md:flex-row md:justify-start md:gap-3 md:px-3 md:text-sm", page === id && "bg-card text-primary shadow-xs hover:bg-card hover:text-primary")}
      onClick={() => setPage(id)}><Icon className="size-4" aria-hidden />{label}</Button>)}
  </nav>;

  return <div className="min-h-screen md:flex">
    <aside className="fixed inset-y-0 left-0 hidden w-48 flex-col border-r bg-sidebar px-4 py-7 md:flex">
      <div className="mb-9 flex items-center gap-2.5 px-2"><img className="size-9" src="/logo.svg" alt="" /><span className="text-xl font-semibold tracking-tight">Lime</span></div>
      {navigation}
      <Button variant="ghost" className="mt-auto justify-start gap-3 px-3 text-xs text-sidebar-foreground" onClick={() => setTheme(theme === "system" ? "light" : theme === "light" ? "dark" : "system")} aria-label={`外观：${theme === "system" ? "跟随系统" : theme === "light" ? "浅色" : "深色"}`}><ThemeIcon className="size-4" />{theme === "system" ? "跟随系统" : theme === "light" ? "浅色" : "深色"}</Button>
    </aside>
    <div className="min-w-0 flex-1 md:ml-48">
      <header className="sticky top-0 z-20 border-b bg-background/95 px-4 sm:px-7">
        <div className="mx-auto flex h-16 max-w-page items-center justify-between gap-3">
          <div className="flex min-w-0 items-center gap-2 md:hidden"><img src="/logo.svg" alt="" className="size-7" /><span className="font-semibold">Lime</span></div>
          <div className="hidden min-w-0 items-center gap-2 text-xs text-muted-foreground md:flex" role="status">{pending && <><LoaderCircle className="size-3.5 animate-spin" />{pending}…</>}</div>
          <div className="flex items-center gap-3">
            <span role="status" className="flex items-center gap-2 text-xs text-muted-foreground" title={error ?? undefined}>
              <span aria-hidden className={cn("size-1.5 rounded-full", state === "ready" ? "bg-success" : state === "unavailable" ? "bg-destructive" : "bg-warning")} />
              {loading && !status && !error ? "正在连接…" : stateLabels[state]}
            </span>
            <Button variant="ghost" size="icon-sm" aria-label="刷新服务状态" disabled={!!pending || loading} onClick={() => void refresh()}><RefreshCw className={cn(loading && "animate-spin")} /></Button>
          </div>
        </div>
        <div className="pb-3 md:hidden">{navigation}</div>
      </header>
      <main className="mx-auto w-full min-w-0 max-w-page px-4 py-6 sm:px-7 sm:py-7">
        <section id="page-settings" aria-label="设置" hidden={page !== "settings"}><SettingsPage /></section>
        <section id="page-test" aria-label="测试" hidden={page !== "test"}><TestPage active={page === "test"} /></section>
        <section id="page-benchmark" aria-label="评测" hidden={page !== "benchmark"}><BenchmarkPage active={page === "benchmark"} /></section>
        <section id="page-dictionary" aria-label="词库" hidden={page !== "dictionary"}><DictionaryPage active={page === "dictionary"} /></section>
        <section id="page-history" aria-label="历史" hidden={page !== "history"}><HistoryPage active={page === "history"} /></section>
      </main>
    </div>
  </div>;
}
