import { useCallback, useEffect, useId, useRef, useState, type FormEvent } from "react";
import { ChartNoAxesCombined, Download, Play, Plus, RefreshCw, Square, Trash2 } from "lucide-react";
import { clearBenchmarkResults, getBenchmarkDataset, getBenchmarkErrors, getBenchmarkStatus, rerunBenchmarkItem, startBenchmark, stopBenchmark } from "@/api/commands";
import type { BenchmarkCorpus, BenchmarkCorpusCell, BenchmarkDatasetView, BenchmarkErrorPage, BenchmarkMode, BenchmarkModelSelection, BenchmarkResult, BenchmarkRunRequest, BenchmarkRunState } from "@/api/types";
import { useManagement } from "@/app/management-context";
import { ConfirmDialog, EmptyState, ErrorState, Field, Pagination, Toolbar } from "@/components/page-parts";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Progress } from "@/components/ui/progress";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { usePollingResource } from "@/hooks/use-polling-resource";
import { downloadJson } from "@/lib/download";
import { benchmarkModeLabel, benchmarkModelLabel, benchmarkResultAccuracy, sortBenchmarkResults } from "@/ui/benchmark";
import { errorMessage, formatDecimal } from "@/ui/format";
import { benchmarkCurrentProgress, buildBenchmarkRequest, type BenchmarkConfigurationDraft } from "./benchmark-form";
import { readBenchmarkSnapshot } from "./benchmark-resource";

const emptyState: BenchmarkRunState = { status: "idle", datasetId: null, datasetName: null, datasetVersion: null, configRevision: null, rimeSnapshotSha256: null, total: 0, completed: 0, results: [], current: null, queue: [], error: null };
const statusLabels: Record<BenchmarkRunState["status"], string> = { idle: "未开始", running: "评测中", stopping: "停止中", cancelled: "已停止", completed: "已完成", failed: "失败" };
interface BenchmarkSnapshot { dataset: BenchmarkDatasetView | null; state: BenchmarkRunState; }
interface BenchmarkDetail { result: BenchmarkResult; cell: BenchmarkCorpusCell; corpusName: string; }

function accuracyLabel(value: number | null): string {
  if (value === null || !Number.isFinite(value)) return "—";
  const normalized = value > 1 ? value / 100 : value;
  return `${formatDecimal(Math.max(0, Math.min(1, normalized)) * 100)}%`;
}
function modelKey(model: BenchmarkModelSelection): string { return model.kind === "rime_only" ? "rime_only" : `preset:${model.name}`; }
function formatEta(seconds: number | null): string {
  if (seconds === null || !Number.isFinite(seconds) || seconds < 0) return "—";
  const rounded = Math.ceil(seconds);
  if (rounded < 60) return `${rounded} 秒`;
  const minutes = Math.floor(rounded / 60);
  if (minutes < 60) return `${minutes} 分${rounded % 60 ? ` ${rounded % 60} 秒` : ""}`;
  return `${Math.floor(minutes / 60)} 小时${minutes % 60 ? ` ${minutes % 60} 分` : ""}`;
}
function cellFor(result: BenchmarkResult, corpusId: string): BenchmarkCorpusCell | null { return result.corpusCells.find((cell) => cell.corpusId === corpusId) ?? null; }
function resultForQueueKey(results: BenchmarkResult[], key: string): BenchmarkResult | null {
  if (!key) return null;
  return results.find((result) => result.corpusCells.some((cell) => cell.id === key)) ?? null;
}
function resultForQueuePosition(results: BenchmarkResult[], position: number): BenchmarkResult | null {
  if (position < 0) return null;
  let offset = position;
  for (const result of results) {
    if (offset < result.corpusCells.length) return result;
    offset -= result.corpusCells.length;
  }
  return null;
}
function queueLabel(item: { key: string; modelName: string | null; corpusId: string | null }, results: BenchmarkResult[], corpora: BenchmarkCorpus[], position = -1): string {
  const result = resultForQueueKey(results, item.key) ?? resultForQueuePosition(results, position);
  const corpus = corpora.find((entry) => entry.id === item.corpusId);
  if (!result) return `${item.modelName ?? "模型"} · ${corpus?.name ?? item.corpusId ?? "语料"}`;
  return `${benchmarkModelLabel(result.model, result.modelName)} · ${benchmarkModeLabel(result.mode)} · ${configurationLabel(result)} · ${corpus?.name ?? item.corpusId ?? "语料"}`;
}
function cellStatusLabel(status: BenchmarkCorpusCell["status"]): string {
  if (status === "completed") return "已完成";
  if (status === "failed") return "失败";
  if (status === "cancelled") return "已停止";
  if (status === "running") return "评测中";
  return "待评测";
}
function configurationLabel(result: BenchmarkResult): string {
  if (result.model?.kind === "rime_only") return "—";
  return `重排 ${result.configuration.llm_rerank_count} · 前文 ${result.configuration.preceding_text_char_limit}`;
}

function CheckOption({ id, label, checked, onChange, disabled = false }: { id: string; label: string; checked: boolean; onChange: (checked: boolean) => void; disabled?: boolean }) {
  return <Label htmlFor={id} className={`flex min-w-0 cursor-pointer items-center gap-2 rounded-lg border px-3 py-2.5 ${checked ? "border-primary/30 bg-accent" : "bg-background"} ${disabled ? "cursor-not-allowed opacity-60" : ""}`}><Checkbox id={id} disabled={disabled} checked={checked} onCheckedChange={(value) => onChange(value === true)} /><span className="min-w-0 break-words">{label}</span></Label>;
}

function DetailDialog({ detail, open, onOpenChange, onRerun }: { detail: BenchmarkDetail | null; open: boolean; onOpenChange: (open: boolean) => void; onRerun: (itemId: string) => void }) {
  const [page, setPage] = useState(1);
  const [errors, setErrors] = useState<BenchmarkErrorPage | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => { setPage(1); setErrors(null); setError(null); }, [detail?.cell.id]);
  useEffect(() => {
    if (!open || !detail) return;
    let current = true;
    setLoading(true); setError(null);
    void getBenchmarkErrors(detail.cell.id, page).then((value) => { if (current) setErrors(value); }).catch((cause) => { if (current) setError(errorMessage(cause)); }).finally(() => { if (current) setLoading(false); });
    return () => { current = false; };
  }, [detail, open, page]);
  return <Dialog open={open} onOpenChange={onOpenChange}><DialogContent className="max-w-detail overflow-y-auto sm:max-w-detail"><DialogHeader><DialogTitle>{detail ? `${detail.corpusName} · ${benchmarkModelLabel(detail.result.model, detail.result.modelName)}` : "评测详情"}</DialogTitle><DialogDescription>{detail ? `${benchmarkModeLabel(detail.result.mode)} · ${configurationLabel(detail.result)}` : ""}</DialogDescription></DialogHeader>
    {detail && <div className="space-y-4"><div className="grid gap-3 sm:grid-cols-4"><div><div className="text-xs text-muted-foreground">准确率</div><div className="font-mono tabular-nums">{accuracyLabel(detail.cell.summary?.accuracy ?? null)}</div></div><div><div className="text-xs text-muted-foreground">总词数</div><div className="font-mono tabular-nums">{detail.cell.summary?.total?.toLocaleString("zh-CN") ?? "—"}</div></div><div><div className="text-xs text-muted-foreground">错误</div><div className="font-mono tabular-nums">{detail.cell.errorCount.toLocaleString("zh-CN")}</div></div><div><div className="text-xs text-muted-foreground">状态</div><div>{cellStatusLabel(detail.cell.status)}</div></div></div>
      {detail.cell.error && <ErrorState message={detail.cell.error} />}{error && <ErrorState message={error} />}{loading && !errors ? <p className="py-6 text-center text-sm text-muted-foreground">正在读取错误记录…</p> : errors?.items.length ? <Table><TableHeader><TableRow>{["上文", "拼音", "目标词", "首位结果", "错误"].map((label) => <TableHead key={label}>{label}</TableHead>)}</TableRow></TableHeader><TableBody>{errors.items.map((item, index) => <TableRow key={`${item.caseId}-${index}`}><TableCell className="max-w-80 whitespace-pre-wrap break-words align-top">{item.context || "（空）"}</TableCell><TableCell className="max-w-48 whitespace-normal break-all align-top font-mono">{item.preedit || "—"}</TableCell><TableCell className="max-w-40 whitespace-normal break-words align-top">{item.expected || "—"}</TableCell><TableCell className="max-w-40 whitespace-normal break-words align-top">{item.top1 || "—"}</TableCell><TableCell className="max-w-64 whitespace-normal break-words align-top text-destructive">{item.error || "首位不匹配"}</TableCell></TableRow>)}</TableBody></Table> : <p className="py-6 text-center text-sm text-muted-foreground">暂无错误记录</p>}{errors && <Pagination page={errors.page} total={errors.total} pageSize={errors.pageSize} onPageChange={setPage} disabled={loading} />}</div>}
    <DialogFooter>{detail && <Button type="button" variant="outline" onClick={() => onRerun(detail.cell.id)} disabled={detail.cell.status === "running"}>{detail.cell.status === "completed" ? "重新测此项" : "开始测此项"}</Button>}<Button type="button" variant="outline" onClick={() => onOpenChange(false)}>关闭</Button></DialogFooter></DialogContent></Dialog>;
}

export function BenchmarkPage({ active }: { active: boolean }) {
  const { presets, pending, connected, run, notify } = useManagement();
  const fieldId = useId();
  const [modes, setModes] = useState<BenchmarkMode[]>(["full"]);
  const [models, setModels] = useState<BenchmarkModelSelection[]>([]);
  const initializedModels = useRef(false);
  const nextConfigurationId = useRef(1);
  const [configurations, setConfigurations] = useState<BenchmarkConfigurationDraft[]>([{ id: "0", rerankCount: "32", contextLimit: "128" }]);
  const [selectedCorpora, setSelectedCorpora] = useState<string[]>([]);
  const [detail, setDetail] = useState<BenchmarkDetail | null>(null);
  const [action, setAction] = useState<"start" | "stop" | "clear" | "rerun" | "refresh" | null>(null);
  const [clearOpen, setClearOpen] = useState(false);
  const actionInFlight = useRef(false);
  const datasetCache = useRef<BenchmarkDatasetView | null>(null);
  const busy = pending !== null || action !== null;
  const load = useCallback(async (): Promise<BenchmarkSnapshot> => { const snapshot = await readBenchmarkSnapshot(() => datasetCache.current ? Promise.resolve(datasetCache.current) : getBenchmarkDataset(), getBenchmarkStatus); datasetCache.current = snapshot.dataset; return snapshot; }, []);
  const resource = usePollingResource({ load, active: active && connected && !busy, intervalMs: 3000 });
  const state = resource.data?.state ?? emptyState;
  const dataset = resource.data?.dataset ?? datasetCache.current;
  const corpora = dataset?.corpora ?? [];
  const running = state.status === "running" || state.status === "stopping";
  const hasPresetModel = models.some((model) => model.kind === "preset");

  const corpusSelectionKey = dataset?.corpora.map((corpus) => corpus.id).join("\u0000") ?? "";
  useEffect(() => { if (!dataset) return; setSelectedCorpora((current) => { const available = new Set(dataset.corpora.map((corpus) => corpus.id)); const next = current.filter((id) => available.has(id)); return next.length ? next : dataset.corpora.map((corpus) => corpus.id); }); }, [dataset?.id, dataset?.version, corpusSelectionKey]);
  useEffect(() => { if (!initializedModels.current && presets.length) { initializedModels.current = true; const loaded = presets.find((preset) => preset.loaded) ?? presets[0]; setModels([{ kind: "preset", name: loaded.name }]); return; } setModels((current) => current.filter((model) => model.kind === "rime_only" || presets.some((preset) => preset.name === model.name))); }, [presets]);
  function setMode(mode: BenchmarkMode, checked: boolean) { setModes((current) => checked ? [...current.filter((value) => value !== mode), mode] : current.filter((value) => value !== mode)); }
  function setModel(model: BenchmarkModelSelection, checked: boolean) { setModels((current) => checked ? [...current.filter((value) => modelKey(value) !== modelKey(model)), model] : current.filter((value) => modelKey(value) !== modelKey(model))); }
  function setCorpus(id: string, checked: boolean) { setSelectedCorpora((current) => checked ? [...new Set([...current, id])] : current.filter((value) => value !== id)); }
  function updateConfiguration(id: string, field: "rerankCount" | "contextLimit", value: string) { setConfigurations((current) => current.map((configuration) => configuration.id === id ? { ...configuration, [field]: value } : configuration)); }
  function addConfiguration() { const id = String(nextConfigurationId.current++); setConfigurations((current) => [...current, { ...current[current.length - 1], id }]); }

  async function start(event: FormEvent<HTMLFormElement>) { event.preventDefault(); if (actionInFlight.current || busy || running || !connected) return; let request: BenchmarkRunRequest; try { request = buildBenchmarkRequest(modes, models, configurations, selectedCorpora); } catch (error) { notify(errorMessage(error), "error"); return; } actionInFlight.current = true; setAction("start"); resource.invalidate(); try { const result = await run("启动评测", () => startBenchmark(request), "评测已开始"); if (result.ok) resource.setData((current) => ({ dataset: current?.dataset ?? datasetCache.current, state: result.value })); } finally { actionInFlight.current = false; setAction(null); void resource.refresh(); } }
  async function stop() { if (actionInFlight.current || busy || state.status !== "running" || !connected) return; actionInFlight.current = true; setAction("stop"); try { const result = await run("停止评测", stopBenchmark); if (result.ok) notify("正在停止评测", "info"); } finally { actionInFlight.current = false; setAction(null); void resource.refresh(); } }
  async function refreshDataset() { if (busy || !connected) return; setAction("refresh"); datasetCache.current = null; try { await resource.refresh(); } finally { setAction(null); } }
  async function clearResults() { setClearOpen(false); setAction("clear"); try { const result = await run("清空评测结果", clearBenchmarkResults); if (result.ok) { notify("已清空评测结果", "info"); await resource.refresh(); } } finally { setAction(null); } }
  async function rerun(itemId: string) { setAction("rerun"); try { const result = await run("重新评测", () => rerunBenchmarkItem(itemId), "已加入评测队列"); if (result.ok) { setDetail(null); await resource.refresh(); } } finally { setAction(null); } }
  function openDetail(result: BenchmarkResult, cell: BenchmarkCorpusCell, corpus: BenchmarkCorpus) { setDetail({ result, cell, corpusName: corpus.name }); }

  const current = state.current;
  const currentProgress = benchmarkCurrentProgress(current);
  const currentCorpus = corpora.find((corpus) => corpus.id === current?.corpusId);
  const currentQueuePosition = current?.itemId ? state.queue.findIndex((item) => item.key === current.itemId) : state.queue.findIndex((item) => item.status === "running");
  const currentResult = current ? resultForQueueKey(state.results, current.itemId ?? "") ?? resultForQueuePosition(state.results, currentQueuePosition) : null;
  const waiting = state.queue.map((item, position) => ({ item, position })).filter(({ item }) => item.status === "waiting");
  const waitingPreview = waiting.slice(0, 6);
  return <div className="space-y-5"><form onSubmit={(event) => void start(event)} className="space-y-5"><Card><CardContent className="space-y-5"><Field label="模型"><div className="flex flex-wrap gap-2" role="group" aria-label="评测模型"><CheckOption id={`${fieldId}-rime`} label="仅 Rime" checked={models.some((model) => model.kind === "rime_only")} onChange={(checked) => setModel({ kind: "rime_only" }, checked)} />{presets.map((preset) => <CheckOption key={preset.name} id={`${fieldId}-model-${encodeURIComponent(preset.name)}`} label={preset.name} checked={models.some((model) => model.kind === "preset" && model.name === preset.name)} onChange={(checked) => setModel({ kind: "preset", name: preset.name }, checked)} />)}{!presets.length && connected && <span className="text-sm text-muted-foreground">尚未添加模型</span>}{!connected && <span className="text-sm text-muted-foreground">等待服务连接</span>}</div></Field><div className="grid gap-5 sm:grid-cols-2"><Field label="拼音"><div className="flex flex-wrap gap-2" role="group" aria-label="拼音模式">{(["full", "initials"] as const).map((mode) => <CheckOption key={mode} id={`${fieldId}-${mode}`} label={benchmarkModeLabel(mode)} checked={modes.includes(mode)} onChange={(checked) => setMode(mode, checked)} />)}</div></Field><Field label="语料"><div className="flex flex-wrap gap-2" role="group" aria-label="评测语料">{corpora.length ? corpora.map((corpus) => <CheckOption key={corpus.id} id={`${fieldId}-corpus-${encodeURIComponent(corpus.id)}`} label={`${corpus.name}${corpus.documents == null ? "" : ` · ${corpus.documents.toLocaleString("zh-CN")} 篇`} · ${corpus.characters.toLocaleString("zh-CN")} 字 · ${corpus.cases.toLocaleString("zh-CN")} 词`} checked={selectedCorpora.includes(corpus.id)} onChange={(checked) => setCorpus(corpus.id, checked)} />) : <span className="text-sm text-muted-foreground">暂无可用语料</span>}</div></Field></div><div className="space-y-3">{configurations.map((configuration, index) => <div key={configuration.id} className="flex items-end gap-3"><div className="grid min-w-0 flex-1 gap-3 sm:grid-cols-2"><Field label="重排候选词数" htmlFor={`${fieldId}-rerank-${configuration.id}`}><Input id={`${fieldId}-rerank-${configuration.id}`} type="number" min={1} max={128} step={1} required={hasPresetModel} disabled={!hasPresetModel} autoFocus={configuration.id !== "0"} value={configuration.rerankCount} onChange={(event) => updateConfiguration(configuration.id, "rerankCount", event.target.value)} /></Field><Field label="前文字符数" htmlFor={`${fieldId}-context-${configuration.id}`}><Input id={`${fieldId}-context-${configuration.id}`} type="number" min={1} max={4096} step={1} required={hasPresetModel} disabled={!hasPresetModel} value={configuration.contextLimit} onChange={(event) => updateConfiguration(configuration.id, "contextLimit", event.target.value)} /></Field></div><Button type="button" variant="ghost" size="icon" disabled={configurations.length <= 1 || !hasPresetModel} aria-label={`移除配置 ${index + 1}`} onClick={() => setConfigurations((current) => current.filter((row) => row.id !== configuration.id))}><Trash2 className="size-4" /></Button></div>)}<Button type="button" variant="outline" size="sm" disabled={!hasPresetModel} onClick={addConfiguration}><Plus className="size-4" />添加配置</Button></div></CardContent></Card><Toolbar><div className="flex flex-wrap gap-2"><Button type="submit" disabled={busy || running || !connected}><Play className="size-4" />开始评测</Button>{running && <Button type="button" variant="outline" disabled={busy || state.status !== "running" || !connected} onClick={() => void stop()}><Square className="size-4" />{state.status === "stopping" ? "停止中" : "停止"}</Button>}</div><div className="flex flex-wrap gap-2"><Button type="button" variant="outline" disabled={busy || !connected} onClick={() => void refreshDataset()}><RefreshCw className={`size-4 ${action === "refresh" ? "animate-spin" : ""}`} />刷新语料</Button><Button type="button" variant="outline" disabled={!state.results.length} onClick={() => downloadJson("lime-benchmark.json", { dataset, ...state })}><Download className="size-4" />导出 JSON</Button><Button type="button" variant="outline" disabled={busy || running || !state.results.length} onClick={() => setClearOpen(true)}>清空结果</Button></div></Toolbar></form>{connected && resource.error && <ErrorState message={resource.error} onRetry={() => void resource.refresh()} />}{dataset?.error && <ErrorState message={dataset.error} onRetry={() => void refreshDataset()} />}{dataset?.directory !== undefined && <div className="break-all text-xs text-muted-foreground">语料目录：{dataset.directory || "（空）"}</div>}{current && (state.status === "running" || state.status === "stopping") && <Card><CardContent className="space-y-3"><div className="flex flex-wrap items-center justify-between gap-3 text-sm"><span>正在测试：{currentResult ? queueLabel({ key: current.itemId ?? "", modelName: current.modelName, corpusId: current.corpusId }, state.results, corpora, currentQueuePosition) : `${benchmarkModelLabel(current.model, current.modelName ?? "")} · ${currentCorpus?.name ?? current.corpusId ?? "语料"}`}</span><span className="font-mono tabular-nums">{current.processed.toLocaleString("zh-CN")} / {current.total.toLocaleString("zh-CN")} · {current.rate == null ? "—" : `${formatDecimal(current.rate)} 词/秒`} · 预计 {formatEta(current.etaSeconds)}</span></div><Progress value={currentProgress} aria-label="当前评测项进度" /><div className="space-y-1 text-xs text-muted-foreground"><div>队列中等待 {waiting.length} 项</div>{waitingPreview.map(({ item, position }) => <div key={`${item.key}-${position}`} className="truncate" title={queueLabel(item, state.results, corpora, position)}>等待：{queueLabel(item, state.results, corpora, position)}</div>)}</div></CardContent></Card>}{state.error && <ErrorState message={state.error} />}{state.results.length ? <Card className="overflow-hidden py-0"><Table><TableHeader><TableRow><TableHead>模型</TableHead><TableHead>配置</TableHead><TableHead>拼音</TableHead>{corpora.map((corpus) => <TableHead key={corpus.id} className="text-right">{corpus.name}</TableHead>)}</TableRow></TableHeader><TableBody>{sortBenchmarkResults(state.results).map((result) => <TableRow key={result.id}><TableCell className="max-w-56 truncate font-medium" title={benchmarkModelLabel(result.model, result.modelName)}>{benchmarkModelLabel(result.model, result.modelName)}</TableCell><TableCell className="text-muted-foreground">{configurationLabel(result)}</TableCell><TableCell><Badge variant="secondary">{benchmarkModeLabel(result.mode)}</Badge></TableCell>{corpora.map((corpus) => { const cell = cellFor(result, corpus.id); return <TableCell key={corpus.id} className="text-right">{cell ? <Button type="button" variant="ghost" size="sm" className="font-mono tabular-nums" onClick={() => openDetail(result, cell, corpus)}>{accuracyLabel(cell.summary?.accuracy ?? benchmarkResultAccuracy(result, corpus.id))}{cell.errorCount ? <span className="ml-1 text-destructive">({cell.errorCount})</span> : null}</Button> : <span className="text-muted-foreground">—</span>}</TableCell>; })}</TableRow>)}</TableBody></Table></Card> : <EmptyState icon={ChartNoAxesCombined} label={resource.loading && !resource.data ? "正在读取评测…" : state.status === "idle" ? "暂无评测结果" : "等待评测结果"} />}{statusLabels[state.status] && state.status !== "idle" && <div className="text-xs text-muted-foreground" aria-live="polite">{statusLabels[state.status]} · 已完成 {state.completed.toLocaleString("zh-CN")} / {state.total.toLocaleString("zh-CN")}</div>}<DetailDialog detail={detail} open={detail !== null} onOpenChange={(open) => { if (!open) setDetail(null); }} onRerun={(itemId) => void rerun(itemId)} /><ConfirmDialog open={clearOpen} onOpenChange={setClearOpen} title="清空评测结果？" description="已完成组合的错误记录和统计会从硬盘删除。" confirmLabel="清空" pending={action === "clear"} onConfirm={() => void clearResults()} /></div>;
}
