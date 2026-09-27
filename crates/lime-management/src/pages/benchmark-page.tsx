import { Fragment, useCallback, useEffect, useId, useRef, useState, type FormEvent } from "react";
import { ChartNoAxesCombined, ChevronDown, ChevronUp, Download, LoaderCircle, Play, Plus, Square, Trash2 } from "lucide-react";
import { getBenchmarkDataset, getBenchmarkStatus, startBenchmark, stopBenchmark } from "@/api/commands";
import type { BenchmarkCorpus, BenchmarkDatasetView, BenchmarkMode, BenchmarkResult, BenchmarkRunRequest, BenchmarkRunState } from "@/api/types";
import { useManagement } from "@/app/management-context";
import { EmptyState, ErrorState, Field, Toolbar } from "@/components/page-parts";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Progress } from "@/components/ui/progress";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { usePollingResource } from "@/hooks/use-polling-resource";
import { downloadJson } from "@/lib/download";
import { benchmarkModeLabel, benchmarkResultAccuracy, sortBenchmarkResults } from "@/ui/benchmark";
import { errorMessage, formatDecimal } from "@/ui/format";
import { benchmarkErrorExamples, benchmarkProgress, buildBenchmarkRequest, type BenchmarkConfigurationDraft } from "./benchmark-form";
import { readBenchmarkSnapshot } from "./benchmark-resource";

const emptyState: BenchmarkRunState = {
  status: "idle", datasetId: null, datasetName: null, datasetVersion: null,
  configRevision: null, rimeSnapshotSha256: null, total: 0, completed: 0, results: [], error: null,
};

const statusLabels: Record<BenchmarkRunState["status"], string> = {
  idle: "未开始", running: "评测中", stopping: "停止中", cancelled: "已停止", completed: "已完成", failed: "失败",
};

const fallbackCorpora: Pick<BenchmarkCorpus, "id" | "name">[] = [
  { id: "zhihu", name: "知乎" }, { id: "classics", name: "经典文章" },
];

interface BenchmarkSnapshot {
  dataset: BenchmarkDatasetView | null;
  state: BenchmarkRunState;
}

function accuracyLabel(value: number | null): string {
  if (value === null || !Number.isFinite(value)) return "—";
  const normalized = value > 1 ? value / 100 : value;
  return `${formatDecimal(Math.max(0, Math.min(1, normalized)) * 100)}%`;
}

function CheckOption({ id, label, checked, onChange }: { id: string; label: string; checked: boolean; onChange: (checked: boolean) => void }) {
  return (
    <Label htmlFor={id} className={`flex min-w-0 cursor-pointer items-center gap-2 rounded-lg border px-3 py-2.5 ${checked ? "border-primary/30 bg-accent" : "bg-background"}`}>
      <Checkbox id={id} checked={checked} onCheckedChange={(value) => onChange(value === true)} />
      <span className="min-w-0 break-words">{label}</span>
    </Label>
  );
}

function ResultErrors({ result, corpora }: { result: BenchmarkResult; corpora: Pick<BenchmarkCorpus, "id" | "name">[] }) {
  const examples = benchmarkErrorExamples(result);
  return (
    <div className="space-y-3 p-2 sm:p-3">
      {result.error && <ErrorState message={result.error} />}
      {examples.length ? (
        <Table>
          <TableHeader>
            <TableRow>
              {["语料", "上文", "拼音", "目标词", "首位结果", "错误"].map((label) => <TableHead key={label}>{label}</TableHead>)}
            </TableRow>
          </TableHeader>
          <TableBody>
            {examples.map((example, index) => (
              <TableRow key={`${example.caseId}-${example.mode}-${index}`}>
                <TableCell className="align-top">{corpora.find((corpus) => corpus.id === example.category)?.name ?? example.category ?? "—"}</TableCell>
                <TableCell className="min-w-48 max-w-80 whitespace-pre-wrap break-words align-top">{example.context || "（空）"}</TableCell>
                <TableCell className="max-w-48 whitespace-normal break-all align-top font-mono">{example.preedit || "—"}</TableCell>
                <TableCell className="max-w-40 whitespace-normal break-words align-top">{example.expected || "—"}</TableCell>
                <TableCell className="max-w-40 whitespace-normal break-words align-top">{example.top1 || "—"}</TableCell>
                <TableCell className="max-w-64 whitespace-normal break-words align-top text-destructive">{example.error || "首位不匹配"}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      ) : <p className="py-3 text-center text-sm text-muted-foreground">{result.status === "completed" ? "暂无错误记录" : "暂无错误样例"}</p>}
    </div>
  );
}

export function BenchmarkPage({ active }: { active: boolean }) {
  const { presets, pending, connected, run, notify } = useManagement();
  const fieldId = useId();
  const [modes, setModes] = useState<BenchmarkMode[]>(["full"]);
  const [models, setModels] = useState<string[]>([]);
  const initializedModels = useRef(false);
  const nextConfigurationId = useRef(1);
  const [configurations, setConfigurations] = useState<BenchmarkConfigurationDraft[]>([
    { id: "0", rerankCount: "32", contextLimit: "128" },
  ]);
  const [expandedResults, setExpandedResults] = useState<Set<string>>(new Set());
  const [action, setAction] = useState<"start" | "stop" | null>(null);
  const actionInFlight = useRef(false);
  const datasetCache = useRef<BenchmarkDatasetView | null>(null);
  const busy = pending !== null || action !== null;
  const load = useCallback(async (): Promise<BenchmarkSnapshot> => {
    const snapshot = await readBenchmarkSnapshot(
      () => datasetCache.current ? Promise.resolve(datasetCache.current) : getBenchmarkDataset(),
      getBenchmarkStatus,
    );
    datasetCache.current = snapshot.dataset;
    return snapshot;
  }, []);
  const resource = usePollingResource({ load, active: active && connected && !busy, intervalMs: 3000 });
  const state = resource.data?.state ?? emptyState;
  const dataset = resource.data?.dataset ?? datasetCache.current;
  const corpora = dataset?.corpora ?? fallbackCorpora;
  const running = state.status === "running" || state.status === "stopping";
  const percentage = benchmarkProgress(state);

  useEffect(() => {
    if (!initializedModels.current && presets.length) {
      initializedModels.current = true;
      setModels([presets.find((preset) => preset.loaded)?.name ?? presets[0].name]);
      return;
    }
    setModels((current) => {
      const next = current.filter((name) => presets.some((preset) => preset.name === name));
      return next.length === current.length ? current : next;
    });
  }, [presets]);

  function setMode(mode: BenchmarkMode, checked: boolean) {
    setModes((current) => checked ? [...current.filter((value) => value !== mode), mode] : current.filter((value) => value !== mode));
  }

  function setModel(name: string, checked: boolean) {
    setModels((current) => checked ? [...current.filter((value) => value !== name), name] : current.filter((value) => value !== name));
  }

  function updateConfiguration(id: string, field: "rerankCount" | "contextLimit", value: string) {
    setConfigurations((current) => current.map((configuration) => configuration.id === id ? { ...configuration, [field]: value } : configuration));
  }

  function addConfiguration() {
    const id = String(nextConfigurationId.current++);
    setConfigurations((current) => [...current, { ...current[current.length - 1], id }]);
  }

  function toggleResult(id: string) {
    setExpandedResults((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  async function start(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (actionInFlight.current || busy || running || !connected) return;
    let request: BenchmarkRunRequest;
    try {
      request = buildBenchmarkRequest(modes, models.filter((name) => presets.some((preset) => preset.name === name)), configurations);
    } catch (error) {
      notify(errorMessage(error), "error");
      return;
    }
    actionInFlight.current = true;
    setAction("start");
    resource.invalidate();
    try {
      const result = await run("启动评测", () => startBenchmark(request), "评测已开始");
      if (result.ok) {
        resource.invalidate();
        resource.setData((current) => ({ dataset: current?.dataset ?? datasetCache.current, state: result.value }));
        setExpandedResults(new Set());
      }
    } finally {
      actionInFlight.current = false;
      setAction(null);
      void resource.refresh();
    }
  }

  async function stop() {
    if (actionInFlight.current || busy || state.status !== "running" || !connected) return;
    actionInFlight.current = true;
    setAction("stop");
    resource.invalidate();
    try {
      const result = await run("停止评测", stopBenchmark);
      if (result.ok) {
        resource.invalidate();
        resource.setData((current) => current ? {
          ...current,
          state: current.state.status === "running" ? { ...current.state, status: "stopping" } : current.state,
        } : current);
        notify("正在停止评测", "info");
      }
    } finally {
      actionInFlight.current = false;
      setAction(null);
      void resource.refresh();
    }
  }

  return (
    <div className="space-y-5">
      <form onSubmit={(event) => void start(event)} className="space-y-5">
        <Card>
          <CardContent className="space-y-5">
            <Field label="模型">
              {presets.length ? (
                <div className="flex flex-wrap gap-2" role="group" aria-label="评测模型">
                  {presets.map((preset) => <CheckOption key={preset.name} id={`${fieldId}-model-${encodeURIComponent(preset.name)}`} label={preset.name} checked={models.includes(preset.name)} onChange={(checked) => setModel(preset.name, checked)} />)}
                </div>
              ) : <p className="text-sm text-muted-foreground">{connected ? "尚未添加模型" : "等待服务连接"}</p>}
            </Field>
            <div className="grid gap-5 sm:grid-cols-2">
              <Field label="拼音">
                <div className="flex flex-wrap gap-2" role="group" aria-label="拼音模式">
                  {(["full", "initials"] as const).map((mode) => <CheckOption key={mode} id={`${fieldId}-${mode}`} label={benchmarkModeLabel(mode)} checked={modes.includes(mode)} onChange={(checked) => setMode(mode, checked)} />)}
                </div>
              </Field>
              <Field label="语料">
                <div className="flex min-h-10 flex-wrap items-center gap-2">
                  {dataset ? dataset.corpora.length ? dataset.corpora.map((corpus) => (
                    <Badge key={corpus.id} variant="secondary">{corpus.name} · {corpus.characters.toLocaleString("zh-CN")} 字</Badge>
                  )) : <span className="text-sm text-muted-foreground">暂无可用语料</span> : <span className="text-sm text-muted-foreground">{connected ? "正在读取语料…" : "等待服务连接"}</span>}
                </div>
              </Field>
            </div>
            <div className="space-y-3">
              {configurations.map((configuration, index) => (
                <div key={configuration.id} className="flex items-end gap-3">
                  <div className="grid min-w-0 flex-1 gap-3 sm:grid-cols-2">
                    <Field label="重排候选词数" htmlFor={`${fieldId}-rerank-${configuration.id}`}>
                      <Input id={`${fieldId}-rerank-${configuration.id}`} type="number" min={1} max={128} step={1} required autoFocus={configuration.id !== "0"} value={configuration.rerankCount} onChange={(event) => updateConfiguration(configuration.id, "rerankCount", event.target.value)} />
                    </Field>
                    <Field label="前文字符数" htmlFor={`${fieldId}-context-${configuration.id}`}>
                      <Input id={`${fieldId}-context-${configuration.id}`} type="number" min={1} max={4096} step={1} required value={configuration.contextLimit} onChange={(event) => updateConfiguration(configuration.id, "contextLimit", event.target.value)} />
                    </Field>
                  </div>
                  <Button type="button" variant="ghost" size="icon" disabled={configurations.length <= 1} aria-label={`移除配置 ${index + 1}`} onClick={() => setConfigurations((current) => current.filter((row) => row.id !== configuration.id))}>
                    <Trash2 className="size-4" />
                  </Button>
                </div>
              ))}
              <Button type="button" variant="outline" size="sm" onClick={addConfiguration}><Plus className="size-4" />添加配置</Button>
            </div>
          </CardContent>
        </Card>
        <Toolbar>
          <Button type="submit" disabled={busy || running || !connected}>
            {action === "start" ? <LoaderCircle className="size-4 animate-spin" /> : <Play className="size-4" />}开始评测
          </Button>
          {running && <Button type="button" variant="outline" disabled={busy || state.status !== "running" || !connected} onClick={() => void stop()}><Square className="size-4" />{state.status === "stopping" ? "停止中" : "停止"}</Button>}
          <Button type="button" variant="outline" disabled={!state.results.length} onClick={() => downloadJson("lime-benchmark.json", { dataset, ...state })}><Download className="size-4" />导出 JSON</Button>
        </Toolbar>
      </form>
      {connected && resource.error && <ErrorState message={resource.error} onRetry={() => void resource.refresh()} />}
      <div className="space-y-2" aria-live="polite">
        <div className="flex items-center justify-between gap-3 text-sm">
          <span className={state.status === "failed" ? "text-destructive" : "text-muted-foreground"}>{statusLabels[state.status]}</span>
          <span className="font-mono tabular-nums">{formatDecimal(percentage)}%</span>
        </div>
        <Progress value={percentage} aria-label="评测进度" />
      </div>
      {state.error && <ErrorState message={state.error} />}
      {state.results.length ? (
        <Card className="overflow-hidden py-0">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>模型</TableHead><TableHead>配置</TableHead><TableHead className="text-right">总准确率</TableHead>
                {corpora.map((corpus) => <TableHead key={corpus.id} className="text-right">{corpus.name}</TableHead>)}
                <TableHead>状态</TableHead><TableHead><span className="sr-only">错误样例</span></TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {sortBenchmarkResults(state.results).map((result) => {
                const expanded = expandedResults.has(result.id);
                const detailId = `${fieldId}-result-${encodeURIComponent(result.id)}`;
                const configurationLabel = `${benchmarkModeLabel(result.mode)} · 重排 ${result.configuration.llm_rerank_count} · 前文 ${result.configuration.preceding_text_char_limit}`;
                return (
                  <Fragment key={result.id}>
                    <TableRow tabIndex={0} aria-expanded={expanded} aria-controls={expanded ? detailId : undefined} data-state={expanded ? "selected" : undefined} className="cursor-pointer focus-visible:bg-accent focus-visible:outline-none" onClick={() => toggleResult(result.id)} onKeyDown={(event) => {
                      if (event.target !== event.currentTarget || (event.key !== "Enter" && event.key !== " ")) return;
                      event.preventDefault();
                      toggleResult(result.id);
                    }}>
                      <TableCell className="max-w-56 truncate font-medium" title={result.modelName}>{result.modelName}</TableCell>
                      <TableCell className="text-muted-foreground">{configurationLabel}</TableCell>
                      <TableCell className="text-right font-mono font-medium tabular-nums">{accuracyLabel(benchmarkResultAccuracy(result))}</TableCell>
                      {corpora.map((corpus) => <TableCell key={corpus.id} className="text-right font-mono tabular-nums">{accuracyLabel(benchmarkResultAccuracy(result, corpus.id))}</TableCell>)}
                      <TableCell><Badge variant={result.status === "failed" ? "destructive" : "secondary"}>{result.status === "pending" || result.status === "idle" ? "待评测" : statusLabels[result.status]}</Badge></TableCell>
                      <TableCell className="text-right"><Button type="button" variant="ghost" size="sm" aria-expanded={expanded} aria-controls={expanded ? detailId : undefined} aria-label={`${expanded ? "收起" : "查看"}${result.modelName}，${configurationLabel}的错误样例`}>{expanded ? "收起" : "查看错误"}{expanded ? <ChevronUp className="size-4" /> : <ChevronDown className="size-4" />}</Button></TableCell>
                    </TableRow>
                    {expanded && <TableRow id={detailId} className="bg-muted/40 hover:bg-muted/40"><TableCell colSpan={5 + corpora.length} className="whitespace-normal p-0"><ResultErrors result={result} corpora={corpora} /></TableCell></TableRow>}
                  </Fragment>
                );
              })}
            </TableBody>
          </Table>
        </Card>
      ) : <EmptyState icon={ChartNoAxesCombined} label={resource.loading && !resource.data ? "正在读取评测…" : state.status === "idle" ? "暂无评测结果" : "等待评测结果"} />}
    </div>
  );
}
