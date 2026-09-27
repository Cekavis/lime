import { useEffect, useRef, useState } from "react";
import { ChevronRight, History, RefreshCw, Trash2 } from "lucide-react";
import { clearHistory, getHistoryPage, waitForHistory } from "@/api/commands";
import { HISTORY_PAGE_SIZE, type Candidate, type HistoryPage as HistoryData, type InputData } from "@/api/types";
import { useManagement } from "@/app/management-context";
import { InputResult } from "@/components/input-result";
import { ConfirmDialog, EmptyState, ErrorState, Pagination, Toolbar } from "@/components/page-parts";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { usePollingResource } from "@/hooks/use-polling-resource";
import { errorMessage, formatMilliseconds, formatTimestamp } from "@/ui/format";
import { createHistoryWatcher, INPUT_TEST_COMPLETED_EVENT, keyedHistoryEntries, sortHistory } from "./history-state";

const subscribeHistory = createHistoryWatcher(waitForHistory);

function previewCandidates(candidates: Candidate[]) {
  const values = candidates.slice(0, 2).map((candidate) => candidate.displayText || candidate.commitText).filter(Boolean);
  return values.length ? values.join(" / ") : "—";
}

function historyCandidates(entry: InputData) {
  const rime = entry.rimeCandidates.length ? entry.rimeCandidates : entry.diagnostics.flatMap((item) => item.rimeCandidate ? [item.rimeCandidate] : []);
  const scored = entry.diagnostics.flatMap((item) => item.llmCandidate ? [item.llmCandidate] : []);
  return { rime: previewCandidates(rime), final: previewCandidates(scored.length ? scored : entry.finalCandidates) };
}

export function HistoryPage({ active }: { active: boolean }) {
  const { config, connected, pending, run } = useManagement();
  const [page, setPage] = useState(1);
  const [selected, setSelected] = useState<{ entry: InputData; effectiveCount: number } | null>(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [clearing, setClearing] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [knownTotal, setKnownTotal] = useState(0);
  const pageRef = useRef(page);
  const clearingRef = useRef(false);
  const refreshButtonRef = useRef<HTMLButtonElement>(null);
  const detailTriggerRef = useRef<HTMLButtonElement | null>(null);
  const detailTitleRef = useRef<HTMLHeadingElement>(null);
  pageRef.current = page;

  const resource = usePollingResource<HistoryData>({
    active: active && !clearing,
    key: String(page),
    load: async () => {
      let response = await getHistoryPage(pageRef.current);
      const lastPage = Math.max(1, Math.ceil(response.total / HISTORY_PAGE_SIZE));
      if (response.page > lastPage) response = await getHistoryPage(lastPage);
      return sortHistory(response);
    },
  });
  const resourceRef = useRef(resource);
  resourceRef.current = resource;

  useEffect(() => {
    const unsubscribe = subscribeHistory({
      onChange: () => {
        if (clearingRef.current) return;
        resourceRef.current.invalidate();
        void resourceRef.current.refresh();
      },
      onError: () => {
        // A normal history read provides the visible, localized error state;
        // the native notification stream retries independently in the background.
        if (!clearingRef.current) void resourceRef.current.refresh();
      },
    });
    const onTestCompleted = () => {
      const previousPage = pageRef.current;
      pageRef.current = 1;
      resourceRef.current.invalidate();
      if (previousPage !== 1) resourceRef.current.setData(undefined);
      setPage(1);
      void resourceRef.current.refresh();
    };
    window.addEventListener(INPUT_TEST_COMPLETED_EVENT, onTestCompleted);
    return () => {
      unsubscribe();
      window.removeEventListener(INPUT_TEST_COMPLETED_EVENT, onTestCompleted);
    };
  }, []);

  useEffect(() => {
    if (resource.data) setKnownTotal(resource.data.total);
    if (resource.data && resource.data.page !== page) {
      pageRef.current = resource.data.page;
      setPage(resource.data.page);
    }
  }, [resource.data, page]);

  function changePage(next: number) {
    if (next === page) return;
    resource.invalidate();
    resource.setData(undefined);
    pageRef.current = next;
    setPage(next);
    setSelected(null);
  }

  async function clear() {
    if (clearingRef.current || pending) return;
    clearingRef.current = true;
    setClearing(true);
    setActionError(null);
    resource.invalidate();
    try {
      const response = await run("清空输入历史", async () => {
        try { await clearHistory(); }
        catch (cause) { setActionError(errorMessage(cause)); throw cause; }
      }, "输入历史已清空");
      if (response.ok) {
        resource.invalidate();
        pageRef.current = 1;
        setPage(1);
        setKnownTotal(0);
        resource.setData({ items: [], total: 0, page: 1, pageSize: HISTORY_PAGE_SIZE });
        setSelected(null);
        setConfirmOpen(false);
      }
    } finally {
      clearingRef.current = false;
      setClearing(false);
      void resourceRef.current.refresh();
    }
  }

  const items = resource.data?.items ?? [];
  const total = resource.data?.total ?? knownTotal;
  const error = actionError ?? resource.error;

  return (
    <div className="space-y-5">
      <Toolbar>
        <span className="text-sm tabular-nums text-muted-foreground">{total} 条记录</span>
        <div className="ml-auto flex items-center gap-2">
          <Button ref={refreshButtonRef} variant="ghost" size="icon" aria-label="刷新输入历史" onClick={() => { setActionError(null); void resource.refresh(); }} disabled={clearing}><RefreshCw className={resource.loading ? "size-4 animate-spin" : "size-4"} aria-hidden="true" /></Button>
          <Button variant="outline" size="sm" onClick={() => setConfirmOpen(true)} disabled={!connected || Boolean(pending) || clearing || total === 0}><Trash2 className="size-4" aria-hidden="true" />清空</Button>
        </div>
      </Toolbar>
      {error && <ErrorState message={error} onRetry={() => { setActionError(null); void resource.refresh(); }} />}
      {items.length ? <Card className="min-w-0 overflow-hidden">
        <CardContent>
          <Table aria-label="输入历史">
            <TableHeader><TableRow>
              <TableHead>拼音</TableHead>
              <TableHead className="hidden lg:table-cell">上文</TableHead>
              <TableHead className="hidden sm:table-cell">Rime 候选</TableHead>
              <TableHead>排序候选</TableHead>
              <TableHead className="text-right">用时</TableHead>
              <TableHead className="hidden xl:table-cell">模型</TableHead>
              <TableHead className="w-12"><span className="sr-only">详情</span></TableHead>
            </TableRow></TableHeader>
            <TableBody>{keyedHistoryEntries(items).map(({ entry, key }) => {
              const candidates = historyCandidates(entry);
              const openDetail = (trigger: HTMLButtonElement | null) => {
                detailTriggerRef.current = trigger;
                setSelected({ entry, effectiveCount: config?.llm_effective_count ?? 3 });
              };
              return <TableRow key={key} className="cursor-pointer" onClick={(event) => openDetail(event.currentTarget.querySelector("button"))}>
                <TableCell className="max-w-32 truncate font-mono" title={entry.preedit}>{entry.preedit || "—"}</TableCell>
                <TableCell className="hidden max-w-48 truncate text-muted-foreground lg:table-cell" title={entry.precedingText}>{entry.precedingText || "（空）"}</TableCell>
                <TableCell className="hidden max-w-40 truncate sm:table-cell" title={candidates.rime}>{candidates.rime}</TableCell>
                <TableCell className="max-w-40 truncate font-medium" title={candidates.final}>{candidates.final}</TableCell>
                <TableCell className="text-right font-mono tabular-nums text-muted-foreground">{formatMilliseconds(entry.endToEndMs)}</TableCell>
                <TableCell className="hidden max-w-40 truncate text-muted-foreground xl:table-cell" title={entry.model ?? undefined}>{entry.model || "—"}</TableCell>
                <TableCell><Button variant="ghost" size="icon" aria-label={`查看 ${entry.preedit || "输入"} 的记录`} onClick={(event) => { event.stopPropagation(); openDetail(event.currentTarget); }}><ChevronRight className="size-4" aria-hidden="true" /></Button></TableCell>
              </TableRow>;
            })}</TableBody>
          </Table>
        </CardContent>
      </Card> : !error && <EmptyState icon={History} label={resource.loading ? "正在加载记录" : "暂无输入记录"} />}
      {total > 0 && <Pagination page={page} total={total} pageSize={HISTORY_PAGE_SIZE} onPageChange={changePage} disabled={clearing} />}
      <Dialog open={selected !== null} onOpenChange={(open) => { if (!open) setSelected(null); }}>
        <DialogContent className="max-w-detail overflow-y-auto sm:max-w-detail" aria-describedby={undefined}
          onOpenAutoFocus={(event) => { event.preventDefault(); detailTitleRef.current?.focus({ preventScroll: true }); }}
          onCloseAutoFocus={(event) => {
            event.preventDefault();
            const target = detailTriggerRef.current?.isConnected ? detailTriggerRef.current : refreshButtonRef.current;
            if (active) target?.focus();
          }}>
          <DialogHeader><DialogTitle ref={detailTitleRef} tabIndex={-1} className="outline-none">输入记录</DialogTitle></DialogHeader>
          {selected && <div className="min-w-0 space-y-5">
            <dl className="grid gap-3 text-sm sm:grid-cols-2">
              <div className="flex min-w-0 gap-3"><dt className="shrink-0 text-muted-foreground">时间</dt><dd>{formatTimestamp(selected.entry.timestampMs)}</dd></div>
              <div className="flex min-w-0 gap-3"><dt className="shrink-0 text-muted-foreground">模型</dt><dd className="break-all">{selected.entry.model || "—"}</dd></div>
              <div className="space-y-2 sm:col-span-2"><dt className="text-muted-foreground">上文</dt><dd className="max-h-40 overflow-y-auto whitespace-pre-wrap break-all rounded-md bg-muted/50 p-3">{selected.entry.precedingText || "（空）"}</dd></div>
            </dl>
            <InputResult data={selected.entry} effectiveCount={selected.effectiveCount} />
          </div>}
        </DialogContent>
      </Dialog>
      <ConfirmDialog open={confirmOpen} onOpenChange={(open) => { if (!clearing) setConfirmOpen(open); }} title="清空所有输入记录？" description="清空后无法恢复。" confirmLabel="清空记录" pending={clearing} onConfirm={() => { void clear(); }} />
    </div>
  );
}
