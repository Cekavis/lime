import "./style.css";

import { clearDictionary, clearHistory, deleteModelPreset, getBenchmarkDataset, getBenchmarkStatus, getConfig, getDictionaryPage, getHistoryPage, getStatus, importDictionary, listModelPresets, loadModel, renameModelPreset, saveModelPreset, selectModelPreset, setConfig, startBenchmark, stopBenchmark, testInput, unloadModel, waitForHistory } from "./api/commands";
import { diagnosticsFrom } from "./api/decode";
import { DICTIONARY_PAGE_SIZE, HISTORY_PAGE_SIZE } from "./api/types";
import type { BenchmarkDatasetView, BenchmarkMode, BenchmarkObservation, BenchmarkReportView, BenchmarkRunState, Candidate, CandidateDiagnostic, Config, ConfigSnapshot, DictionaryEntry, DictionaryPage, HistoryPage, InputData, LlmPerformance, ModelInfo, ModelPreset, ServiceState, ServiceStatus } from "./api/types";
import { errorMessage, escapeHtml, formatBytes, formatDecimal, formatLogprobs, formatMilliseconds, formatTimestamp } from "./ui/format";
import { renderAppTemplate } from "./app/template";
type RefreshReason = "initial" | "manual" | "poll" | "tab" | "visibility" | "mutation";

const REFRESH_INTERVAL_MS = 3000;
const HISTORY_WATCH_RETRY_MS = 1000;

const stateLabel: Record<ServiceState, string> = {
  ready: "可用",
  rime_only: "基础模式",
  reloading: "重载中",
  unavailable: "不可用",
};

const defaultConfig: Config = {
  rime_schema: "rime_ice",
  preceding_text_char_limit: 128,
  context_preview_char_limit: 32,
  page_size: 9,
  llm_rerank_count: 32,
  llm_effective_count: 3,
  llm_context_token_limit: 1024,
  llm_inference_count_limit: 1,
  llm_ignore_emoji: true,
  llm_backend: "cuda",
};

type ThemeMode = "system" | "light" | "dark";
const THEME_STORAGE_KEY = "lime.management.theme";

const NOTICE_DURATION_MS = 4000;
const app = document.querySelector<HTMLDivElement>("#app");
if (!app) throw new Error("Lime UI mount point is missing");

app.innerHTML = renderAppTemplate();

let currentConfig: Config = { ...defaultConfig };
let currentStatus: ServiceStatus | null = null;
let currentHistory: InputData[] = [];
let currentHistoryPage = 1;
let currentHistoryTotal = 0;
let currentPresets: ModelPreset[] = [];
let benchmarkDataset: BenchmarkDatasetView | null = null;
let benchmarkDatasetLoaded = false;
let benchmarkState: BenchmarkRunState = {
  status: "idle",
  runId: null,
  datasetId: null,
  datasetName: null,
  datasetVersion: null,
  configRevision: null,
  modelName: null,
  modelSha256: null,
  config: null,
  total: 0,
  completed: 0,
  report: null,
  error: null,
};
let benchmarkRenderedReportKey = "";
let benchmarkRefreshInFlight: Promise<void> | null = null;
let benchmarkRefreshQueued = false;
let benchmarkRefreshEpoch = 0;
let benchmarkMutationEpoch = 0;

let configFormDirty = false;
let pendingConfigSnapshot: ConfigSnapshot | null = null;
let pendingPresets: ModelPreset[] | null = null;
let renderedDictionaryKey = "";
let renderedHistoryKey = "";
let renderedPresetsKey = "";
let refreshInFlight: Promise<void> | null = null;
let refreshQueued = false;
let queuedRefreshReason: RefreshReason | null = null;
let mutationEpoch = 0;
let historyRevision = 0;
let historyRefreshEpoch = 0;
let historyWatchStopped = false;
let noticeTimer: number | null = null;

const query = <T extends Element>(selector: string) => document.querySelector<T>(selector);
const all = <T extends Element>(selector: string) => [...document.querySelectorAll<T>(selector)];

function normalizeThemeMode(value: unknown): ThemeMode {
  return value === "light" || value === "dark" || value === "system" ? value : "system";
}

function storedThemeMode(): ThemeMode {
  try {
    return normalizeThemeMode(window.localStorage.getItem(THEME_STORAGE_KEY));
  } catch {
    return "system";
  }
}

function applyThemeMode(value: unknown, persist = false) {
  const mode = normalizeThemeMode(value);
  document.documentElement.dataset.theme = mode;
  const select = query<HTMLSelectElement>("[data-theme-mode]");
  if (select && select.value !== mode) select.value = mode;
  if (persist) {
    try {
      window.localStorage.setItem(THEME_STORAGE_KEY, mode);
    } catch {
      // The management UI still applies the theme for this session when storage is unavailable.
    }
  }
}

applyThemeMode(storedThemeMode());

function isFocusedWithin(selector: string): boolean {
  const container = query<HTMLElement>(selector);
  const focused = document.activeElement;
  return Boolean(container && focused instanceof Node && container.contains(focused));
}

function markMutation() {
  mutationEpoch += 1;
  if (refreshInFlight) refreshQueued = true;
}

function setNotice(message: string, tone: "success" | "error" | "info" = "info") {
  const notice = query<HTMLDivElement>("[data-notice]");
  if (!notice) return;
  if (noticeTimer !== null) {
    window.clearTimeout(noticeTimer);
    noticeTimer = null;
  }
  if (message.toLowerCase().includes("lime service is unavailable")) message = "";
  const messageTarget = query<HTMLElement>("[data-notice-message]");
  if (messageTarget) messageTarget.textContent = message;
  else notice.textContent = message;
  notice.dataset.tone = tone;
  notice.classList.toggle("is-hidden", !message);
  notice.setAttribute("aria-hidden", String(!message));
  if (message) {
    noticeTimer = window.setTimeout(() => {
      noticeTimer = null;
      notice.classList.add("is-hidden");
      notice.setAttribute("aria-hidden", "true");
    }, NOTICE_DURATION_MS);
  }
}

function recordOperation(message: string) {
  const target = query<HTMLElement>("[data-last-operation]");
  if (target) target.textContent = message;
}

async function fetchHistoryPage(page: number): Promise<HistoryPage> {
  return sortHistory(await getHistoryPage(page));
}

async function fetchDictionaryPage(page: number): Promise<DictionaryPage> {
  return getDictionaryPage(page);
}

async function fetchAllDictionaryEntries(): Promise<DictionaryEntry[]> {
  const first = await fetchDictionaryPage(1);
  const totalPages = Math.max(1, Math.ceil(first.total / DICTIONARY_PAGE_SIZE));
  const entries = [...first.items];
  for (let page = 2; page <= totalPages; page += 1) entries.push(...(await fetchDictionaryPage(page)).items);
  return entries;
}
function sortHistory(page: HistoryPage): HistoryPage {
  const indexed = page.items.map((item, index) => ({ item, index }));
  indexed.sort((left, right) => {
    const leftTime = left.item.timestampMs;
    const rightTime = right.item.timestampMs;
    if (leftTime != null && rightTime != null && leftTime !== rightTime) return rightTime - leftTime;
    // request_id is retained only for wire compatibility. History ordering is determined by the
    // monotonic timestamp supplied by the service; when an older response has no timestamp, keep
    // the server-provided order rather than inventing a request-id ordering.
    return left.index - right.index;
  });
  return { ...page, items: indexed.map((entry) => entry.item) };
}

function renderStatus(status: ServiceStatus | null) {
  const badge = query<HTMLElement>("[data-service-state]");
  const state = status?.state ?? "unavailable";
  if (badge) {
    badge.textContent = "服务 " + stateLabel[state];
    badge.dataset.serviceState = state;
  }
  const stateText = query<HTMLElement>("[data-diagnostic-state]");
  if (stateText) stateText.textContent = stateLabel[state];
  renderModel(status?.model ?? null);
}

function renderModel(model: ModelInfo | null) {
  const loaded = Boolean(model?.loaded);
  const state = query<HTMLElement>("[data-model-state]");
  if (state) {
    state.textContent = loaded ? "已加载" : "未加载";
    state.dataset.loaded = String(loaded);
  }
  const path = query<HTMLElement>("[data-model-path]");
  if (path) {
    const fullPath = model?.path ?? "—";
    path.textContent = fullPath === "—" ? fullPath : truncatePath(fullPath);
    path.title = fullPath;
  }
  const size = query<HTMLElement>("[data-model-size]");
  if (size) size.textContent = model?.sizeBytes == null ? "—" : formatBytes(model.sizeBytes);
  const scoringPath = query<HTMLElement>("[data-model-scoring-path]");
  if (scoringPath) {
    scoringPath.textContent = !loaded || model?.scoringPath == null
      ? "—"
      : model.scoringPath === "attention" ? "Attention" : "Recurrent";
  }
  const memory = query<HTMLElement>("[data-model-memory]");
  if (memory) {
    const entries = loaded && model ? Object.entries(model.memory) : [];
    if (!entries.length) {
      memory.innerHTML = '<p class="muted">' + (loaded ? "llama.cpp 未返回初始化显存信息。" : "未加载模型，暂无显存占用信息。") + "</p>";
    } else {
      const row = ([key, value]: [string, number]) => "<dt>" + escapeHtml(memoryLabel(key)) + "</dt><dd>" + escapeHtml(formatBytes(value)) + "</dd>";
      memory.innerHTML = '<dl class="status-list memory-list">' + entries.map(row).join("") + "</dl>";
    }
  }
  const diagnostic = query<HTMLElement>("[data-diagnostic-model]");
  if (diagnostic) diagnostic.textContent = loaded ? model?.path ?? "已加载" : "未加载（基础模式）";
  for (const preset of currentPresets) preset.loaded = Boolean(loaded && preset.path && model?.path && preset.path === model.path);
  renderModelPresets(currentPresets);
}

function truncatePath(value: string, maxLength = 64): string {
  if (value.length <= maxLength) return value;
  const side = Math.max(8, Math.floor((maxLength - 1) / 2));
  return value.slice(0, side) + "…" + value.slice(-side);
}

function memoryLabel(value: string): string {
  const qualified = value.match(/^([^\.]+)\.(.+)$/);
  if (qualified) {
    const device = qualified[1].toUpperCase();
    const kind = memoryLabel(qualified[2]);
    return device + " · " + kind;
  }
  const label = value
    .replace(/[._-]+/g, " ")
    .replace(/([a-z])([A-Z])/g, "$1 $2")
    .trim();
  const aliases: Record<string, string> = {
    model: "模型",
    model_bytes: "模型权重",
    context: "上下文",
    context_bytes: "上下文",
    kv: "KV 缓存",
    kv_bytes: "KV 缓存",
    kvCache: "KV 缓存",
    compute: "计算缓冲",
    compute_bytes: "计算缓冲",
    buffer: "缓冲区",
    total: "总计",
    total_bytes: "总计",
    output: "输出缓冲",
    output_bytes: "输出缓冲",
    rs: "RS 缓冲",
    rs_bytes: "RS 缓冲",
    lora: "LoRA 缓冲",
    lora_bytes: "LoRA 缓冲",
    state: "状态缓冲",
    state_bytes: "状态缓冲",
    gpu: "GPU",
    vram: "显存",
    backend: "后端",
  };
  const alias = aliases[value] ?? aliases[label] ?? label;
  return alias ? alias.charAt(0).toUpperCase() + alias.slice(1) : "显存";
}

function applyConfig(snapshot: ConfigSnapshot, options: { force?: boolean } = {}) {
  currentConfig = { ...defaultConfig, ...snapshot.config };
  const shouldDefer = !options.force && (configFormDirty || isFocusedWithin("[data-config-form]"));
  if (shouldDefer) {
    pendingConfigSnapshot = snapshot;
    return;
  }
  pendingConfigSnapshot = null;
  for (const input of all<HTMLInputElement | HTMLSelectElement>("[data-config]")) {
    const key = input.dataset.config as keyof Config | undefined;
    if (!key) continue;
    if (input instanceof HTMLInputElement && input.type === "checkbox") {
      const nextValue = Boolean(currentConfig[key]);
      if (input.checked !== nextValue) input.checked = nextValue;
    } else {
      const nextValue = String(currentConfig[key]);
      if (input.value !== nextValue) input.value = nextValue;
    }
  }
  configFormDirty = false;
}

function readConfig(): Config {
  const result = { ...currentConfig };
  for (const input of all<HTMLInputElement | HTMLSelectElement>("[data-config]")) {
    const key = input.dataset.config as keyof Config | undefined;
    if (!key) continue;
    if (input instanceof HTMLInputElement && input.type === "checkbox") result[key] = input.checked as never;
    else if (input instanceof HTMLSelectElement) result[key] = input.value as never;
    else result[key] = Number(input.value) as never;
  }
  return result;
}

function renderDictionary(entries: DictionaryEntry[], options: { force?: boolean } = {}, total = entries.length) {
  const key = JSON.stringify([total, entries]);
  const table = query<HTMLTableSectionElement>("[data-dictionary-table]");
  if (!options.force && key === renderedDictionaryKey) {
    const count = query<HTMLElement>("[data-dictionary-count]");
    if (count) count.textContent = total + " 条";
    const diagnostic = query<HTMLElement>("[data-diagnostic-dictionary]");
    if (diagnostic) diagnostic.textContent = total + " 条";
    return;
  }
  const count = query<HTMLElement>("[data-dictionary-count]");
  if (count) count.textContent = total + " 条";
  const diagnostic = query<HTMLElement>("[data-diagnostic-dictionary]");
  if (diagnostic) diagnostic.textContent = total + " 条";
  if (!table) return;
  const rows = entries.slice(0, 50).map((entry) => "<tr><td>" + escapeHtml(entry.pinyin) + "</td><td>" + escapeHtml(entry.text) + "</td><td>" + entry.weight + "</td></tr>");
  table.innerHTML = rows.length ? rows.join("") : '<tr><td colspan="3" class="muted">词库为空</td></tr>';
  renderedDictionaryKey = key;
}

function candidateText(candidates: Candidate[], limit = 2): string {
  const values = candidates.slice(0, limit).map((candidate) => candidate.displayText).filter(Boolean);
  return values.length ? values.join(" / ") : "—";
}

function historyEntryKey(entry: InputData, index: number): string {
  if (entry.timestampMs != null) return "timestamp:" + entry.timestampMs;
  if (entry.requestId != null) return "request:" + entry.requestId;
  return "fallback:" + index + ":" + entry.precedingText + ":" + entry.preedit;
}

function historyRowContent(entry: InputData): string {
  const rime = entry.rimeCandidates.length ? entry.rimeCandidates : entry.diagnostics.map((item) => item.rimeCandidate).filter((item): item is Candidate => item !== null);
  const diagnosticLlm = entry.diagnostics.map((item) => item.llmCandidate).filter((item): item is Candidate => item !== null);
  const llm = diagnosticLlm.length ? diagnosticLlm : entry.finalCandidates;
  return '<td>' + escapeHtml(entry.precedingText || "（空）") + '</td><td class="mono">' + escapeHtml(entry.preedit || "—") + "</td><td>" + escapeHtml(candidateText(rime)) + "</td><td>" + escapeHtml(candidateText(llm)) + "</td><td class=\"mono numeric\">" + escapeHtml(formatMilliseconds(entry.endToEndMs)) + "</td><td>" + escapeHtml(entry.model || "—") + "</td>";
}

function renderHistoryTable(table: HTMLTableSectionElement, page: HistoryPage) {
  const existing = new Map<string, HTMLTableRowElement>();
  for (const row of [...table.querySelectorAll<HTMLTableRowElement>("tr[data-history-key]")]) {
    if (row.dataset.historyKey) existing.set(row.dataset.historyKey, row);
  }
  const rows: HTMLTableRowElement[] = [];
  page.items.forEach((entry, index) => {
    const key = historyEntryKey(entry, index);
    const row = existing.get(key) ?? document.createElement("tr");
    row.className = "history-row";
    row.tabIndex = 0;
    row.setAttribute("role", "button");
    row.dataset.historyKey = key;
    row.dataset.historyIndex = String(index);
    const content = historyRowContent(entry);
    if (row.innerHTML !== content) row.innerHTML = content;
    rows.push(row);
  });
  if (!rows.length) {
    table.innerHTML = '<tr><td colspan="6" class="muted">暂无输入记录</td></tr>';
    return;
  }
  // Moving existing rows instead of replacing the whole table preserves the
  // focused row and pointer interaction while prepending a new entry.
  table.replaceChildren(...rows);
}

function effectiveDisplayCandidate(diagnostic: CandidateDiagnostic, rowIndex: number, data: InputData): Candidate | null {
  if (diagnostic.hasDisplayCandidate) return diagnostic.displayCandidate;
  const effectiveCount = Math.max(0, Math.trunc(currentConfig.llm_effective_count || 0));
  if (data.diagnostics.length === 0 || rowIndex < effectiveCount) return diagnostic.llmCandidate ?? data.finalCandidates[rowIndex] ?? null;
  return null;
}

function diagnosticRows(data: InputData): string {
  const diagnostics = data.diagnostics.length ? data.diagnostics : diagnosticsFrom([], data.rimeCandidates, data.finalCandidates);
  if (!diagnostics.length) return '<tr><td colspan="7" class="muted">暂无候选</td></tr>';
  return diagnostics.map((diagnostic, rowIndex) => {
    const display = effectiveDisplayCandidate(diagnostic, rowIndex, data);
    const scored = diagnostic.llmCandidate !== null && diagnostic.logprobs.length > 0 && diagnostic.logprob != null;
    const logprob = !scored ? "—" : formatDecimal(diagnostic.logprob!);
    const logprobs = !scored ? "—" : formatLogprobs(diagnostic.logprobs, diagnostic.logprob);
    const mismatch = !scored || diagnostic.mismatch == null ? "—" : diagnostic.mismatch ? "是" : "否";
    const mismatchClass = !scored || diagnostic.mismatch == null ? "" : diagnostic.mismatch ? " is-mismatch" : " is-match";
    return "<tr><td class=\"mono\">" + (rowIndex + 1) + "</td><td>" + candidateCell(diagnostic.rimeCandidate) + "</td><td>" + candidateCell(diagnostic.llmCandidate) + "</td><td class=\"mono numeric\">" + logprob + "</td><td class=\"mono logprobs\">" + logprobs + "</td><td class=\"mismatch" + mismatchClass + "\">" + mismatch + "</td><td>" + candidateCell(display) + "</td></tr>";
  }).join("");
}

function diagnosticTable(data: InputData): string {
  return '<div class="table-wrap diagnostic-table-wrap"><table class="diagnostic-table"><thead><tr><th>#</th><th>Rime 原始候选</th><th>LLM 候选</th><th>Logprob</th><th>Logprobs</th><th>Mismatch</th><th>展示候选</th></tr></thead><tbody>' + diagnosticRows(data) + "</tbody></table></div>";
}

function llmPerformanceSummary(performance: LlmPerformance | null, rimeMs: number | null, endToEndMs: number | null): string {
  if (!performance && rimeMs == null && endToEndMs == null) return '<p class="muted">本次未调用 LLM。</p>';
  const row = (label: string, value: string) => '<dt>' + label + '</dt><dd class="mono">' + escapeHtml(value) + '</dd>';
  const effectiveRimeMs = rimeMs ?? performance?.rimeMs ?? null;
  const rows = [
    row("端到端用时", formatMilliseconds(endToEndMs)),
    row("Rime 用时", formatMilliseconds(effectiveRimeMs)),
    row("推理用时", formatMilliseconds(performance?.decodeMs)),
    row("送入候选", performance ? String(performance.candidateCount) + " 个" : "—"),
    row("返回得分", performance ? String(performance.scoredCount) + " 个" : "—"),
    row("目标 Token", performance ? String(performance.targetTokenCount) : "—"),
    row("解码批次", performance ? String(performance.batchCount) : "—"),
    row("边界不匹配", performance ? String(performance.mismatchCount) + " 个" : "—"),
    row("上下文 Token", performance ? String(performance.contextTokenCount) : "—"),
    row("Decode 输入行", performance ? String(performance.decodeInputTokenCount) : "—"),
    row("Logprob 输出行", performance ? String(performance.logprobOutputCount) : "—"),
  ];
  const splitAt = Math.ceil(rows.length / 2);
  const columns = [rows.slice(0, splitAt), rows.slice(splitAt)];
  const omitted = performance && performance.omittedCandidateCount > 0
    ? '<p class="muted history-inference-note">有 ' + performance.omittedCandidateCount + ' 个较长候选项由于单次输入推理次数上限没有被纳入排序。</p>'
    : '';
  return '<div class="history-performance">' +
    columns.map((column) => '<dl class="status-list history-performance-column">' + column.join("") + '</dl>').join("") +
    '</div>' + omitted;
}

function candidateCell(candidate: Candidate | null): string {
  return candidate ? escapeHtml(candidate.displayText || candidate.commitText) : "—";
}

function renderTestResult(data: InputData) {
  const target = query<HTMLElement>("[data-test-result]");
  const request = query<HTMLElement>("[data-test-request]");
  if (request) request.textContent = data.preedit ? "已完成：" + data.preedit : "已完成";
  if (!target) return;
  const status = stateLabel[data.serviceState] || "—";
  const context = data.contextUsed == null ? "—" : data.contextUsed ? "是" : "否";
  target.innerHTML = '<div class="result-summary"><span>服务状态</span><strong>' + escapeHtml(status) + '</strong><span>上文已使用</span><strong>' + context + '</strong><span>拼音</span><strong class="mono">' + escapeHtml(data.preedit || "—") + "</strong></div>" + llmPerformanceSummary(data.llmPerformance, data.rimeMs, data.endToEndMs) + diagnosticTable(data);
}

function renderHistory(page: HistoryPage, options: { force?: boolean } = {}) {
  page = sortHistory(page);
  const key = JSON.stringify(page);
  currentHistory = page.items;
  currentHistoryPage = page.page;
  currentHistoryTotal = page.total;
  const count = query<HTMLElement>("[data-history-count]");
  if (count) count.textContent = page.total + " 条";
  const table = query<HTMLTableSectionElement>("[data-history-table]");
  if (table && (options.force || key !== renderedHistoryKey)) {
    renderHistoryTable(table, page);
  }
  renderedHistoryKey = key;
  const totalPages = Math.max(1, Math.ceil(page.total / HISTORY_PAGE_SIZE));
  const visiblePage = Math.min(Math.max(1, page.page), totalPages);
  if (visiblePage !== page.page) currentHistoryPage = visiblePage;
  const label = query<HTMLElement>("[data-history-page-label]");
  if (label) label.textContent = "第 " + visiblePage + " / " + totalPages + " 页";
  const previous = query<HTMLButtonElement>("[data-history-prev]");
  const next = query<HTMLButtonElement>("[data-history-next]");
  if (previous) previous.disabled = visiblePage <= 1;
  if (next) next.disabled = visiblePage >= totalPages;
}

const benchmarkStatusLabels: Record<BenchmarkRunState["status"], string> = {
  idle: "未开始",
  running: "评测中",
  stopping: "停止中",
  cancelled: "已停止",
  completed: "已完成",
  failed: "失败",
};

function benchmarkModeLabel(mode: BenchmarkMode): string {
  return mode === "full" ? "全拼" : "首拼";
}

function benchmarkCategoryLabel(category: string | null): string {
  return category && category.trim() ? category : "未分类";
}

function benchmarkTabActive(): boolean {
  return query<HTMLElement>(".shell")?.dataset.activeTab === "benchmark";
}

function renderBenchmarkDataset(dataset: BenchmarkDatasetView) {
  const target = query<HTMLElement>("[data-benchmark-corpora]");
  if (!target) return;
  const counts = new Map<string, number>();
  for (const item of dataset.cases) {
    const category = item.category ?? "";
    counts.set(category, (counts.get(category) ?? 0) + 1);
  }
  const categories = [...counts.keys()].sort((left, right) => benchmarkCategoryLabel(left).localeCompare(benchmarkCategoryLabel(right), "zh-CN"));
  const previous = new Set(all<HTMLInputElement>("[data-benchmark-category]").filter((input) => input.checked).map((input) => input.value));
  if (!categories.length) {
    target.innerHTML = '<p class="muted">暂无可用语料。</p>';
    return;
  }
  const preserveSelection = previous.size > 0;
  target.innerHTML = categories.map((category) => {
    const value = escapeHtml(category);
    const checked = preserveSelection ? previous.has(category) : true;
    return '<label class="check-option"><input data-benchmark-category type="checkbox" value="' + value + '"' + (checked ? " checked" : "") + ' /><span>' + escapeHtml(benchmarkCategoryLabel(category)) + ' <small class="muted">' + counts.get(category) + ' 条</small></span></label>';
  }).join("");
}

function benchmarkAccuracy(value: number | null): string {
  if (value == null || !Number.isFinite(value)) return "—";
  const normalized = value > 1 ? value / 100 : value;
  return formatDecimal(Math.max(0, Math.min(1, normalized)) * 100) + "%";
}

function benchmarkSummaryRows(report: BenchmarkReportView): string {
  if (!report.summaries.length) return '<tr><td colspan="8" class="muted">暂无汇总</td></tr>';
  return report.summaries.map((summary) => {
    return '<tr><td>' + escapeHtml(benchmarkCategoryLabel(summary.category)) + '</td><td>' + benchmarkModeLabel(summary.mode) + '</td><td class="mono numeric">' + summary.total + '</td><td class="mono numeric">' + summary.completed + '</td><td class="mono numeric">' + summary.correct + '</td><td class="mono numeric">' + summary.noPrediction + '</td><td class="mono numeric">' + summary.errors + '</td><td class="mono numeric">' + benchmarkAccuracy(summary.accuracy) + '</td></tr>';
  }).join("");
}

function benchmarkObservationRows(observations: BenchmarkObservation[]): string {
  return observations.map((observation) => {
    const outcome = observation.error ? "错误" : observation.correct === true ? "正确" : observation.correct === false ? "不正确" : "—";
    const outcomeClass = observation.error || observation.correct === false ? "benchmark-observation-error" : observation.correct === true ? "benchmark-observation-match" : "";
    const errorText = observation.error ? "：" + observation.error : "";
    return '<tr class="' + outcomeClass + '"><td>' + escapeHtml(benchmarkCategoryLabel(observation.category)) + '</td><td>' + benchmarkModeLabel(observation.mode) + '</td><td>' + escapeHtml(observation.context || "（空）") + '</td><td class="mono">' + escapeHtml(observation.preedit || "—") + '</td><td>' + escapeHtml(observation.expected || "—") + '</td><td>' + escapeHtml(observation.top1 || "—") + '</td><td>' + escapeHtml(outcome + errorText) + '</td></tr>';
  }).join("");
}

function renderBenchmarkReport(report: BenchmarkReportView, state: BenchmarkRunState) {
  const key = JSON.stringify(report);
  if (key === benchmarkRenderedReportKey) return;
  benchmarkRenderedReportKey = key;
  const summary = query<HTMLElement>("[data-benchmark-summary]");
  const details = query<HTMLElement>("[data-benchmark-details]");
  const datasetLabel = report.datasetName || state.datasetName;
  const modelLabel = state.modelName;
  const configLabel = state.config
    ? "重排 " + state.config.llm_rerank_count + " · 采纳 " + state.config.llm_effective_count + " · 上下文 " + state.config.llm_context_token_limit
    : "";
  if (summary) {
    const meta = [datasetLabel ? "语料集：" + datasetLabel : "", modelLabel ? "模型：" + modelLabel : "", configLabel ? "配置：" + configLabel : ""].filter(Boolean).join(" · ");
    const runError = state.error ? '<p class="notice-inline error">' + escapeHtml(state.error) + '</p>' : "";
    summary.innerHTML = runError + (meta ? '<p class="benchmark-report-meta muted">' + escapeHtml(meta) + '</p>' : "") + '<div class="table-wrap benchmark-summary-wrap"><table class="benchmark-summary-table"><thead><tr><th>语料</th><th>模式</th><th>样本</th><th>完成</th><th>正确</th><th>无结果</th><th>错误</th><th>准确率</th></tr></thead><tbody>' + benchmarkSummaryRows(report) + '</tbody></table></div>';
  }
  if (!details) return;
  const failed = report.observations.filter((observation) => observation.error || observation.correct === false);
  const examples = (failed.length ? failed : report.observations).slice(0, 40);
  details.innerHTML = examples.length
    ? '<div class="table-wrap benchmark-observations-wrap"><table class="benchmark-observations-table"><thead><tr><th>语料</th><th>模式</th><th>上文</th><th>拼音</th><th>目标</th><th>首位结果</th><th>结果</th></tr></thead><tbody>' + benchmarkObservationRows(examples) + '</tbody></table></div>'
    : '<p class="muted">暂无错误记录。</p>';
}

function renderBenchmarkState(state: BenchmarkRunState) {
  const badge = query<HTMLElement>("[data-benchmark-status]");
  if (badge) {
    badge.textContent = benchmarkStatusLabels[state.status];
    badge.dataset.benchmarkState = state.status;
  }
  const total = Math.max(0, state.total);
  const completed = Math.max(0, Math.min(total || state.completed, state.completed));
  const percentage = total > 0 ? Math.max(0, Math.min(100, completed / total * 100)) : 0;
  const bar = query<HTMLElement>("[data-benchmark-progress-bar]");
  if (bar) bar.style.width = percentage + "%";
  const label = query<HTMLElement>("[data-benchmark-progress-label]");
  if (label) label.textContent = state.status === "failed" ? "评测失败" : benchmarkStatusLabels[state.status];
  const count = query<HTMLElement>("[data-benchmark-progress-count]");
  if (count) count.textContent = completed + " / " + total;
  const run = query<HTMLButtonElement>("[data-benchmark-run]");
  if (run) run.disabled = state.status === "running" || state.status === "stopping";
  const stop = query<HTMLButtonElement>("[data-benchmark-stop]");
  if (stop) stop.disabled = state.status !== "running";
  const exportButton = query<HTMLButtonElement>("[data-benchmark-export]");
  if (exportButton) exportButton.disabled = state.report === null;
  if (state.report) renderBenchmarkReport(state.report, state);
  else if (state.status === "failed") {
    const summary = query<HTMLElement>("[data-benchmark-summary]");
    if (summary) summary.innerHTML = '<p class="notice-inline error">' + escapeHtml(state.error || "评测失败") + '</p>';
  }
}

function renderBenchmarkReadError(message: string) {
  const target = query<HTMLElement>("[data-benchmark-corpora]");
  if (target && !benchmarkDatasetLoaded) target.innerHTML = '<p class="notice-inline error">' + escapeHtml(message) + '</p>';
  const summary = query<HTMLElement>("[data-benchmark-summary]");
  if (summary && !benchmarkState.report) summary.innerHTML = '<p class="notice-inline error">' + escapeHtml(message) + '</p>';
}

function clearBenchmarkReportView(message = "评测进行中，完成后显示结果。") {
  const summary = query<HTMLElement>("[data-benchmark-summary]");
  if (summary) summary.innerHTML = '<p class="muted">' + escapeHtml(message) + '</p>';
  const details = query<HTMLElement>("[data-benchmark-details]");
  if (details) details.innerHTML = "";
}

async function performBenchmarkRefresh(epoch: number, reason: RefreshReason) {
  const mutationAtStart = benchmarkMutationEpoch;
  if (!benchmarkDatasetLoaded) {
    const dataset = await getBenchmarkDataset();
    if (epoch !== benchmarkRefreshEpoch || mutationAtStart !== benchmarkMutationEpoch) return;
    benchmarkDataset = dataset;
    benchmarkDatasetLoaded = true;
    renderBenchmarkDataset(dataset);
  }
  const state = await getBenchmarkStatus();
  if (epoch !== benchmarkRefreshEpoch || mutationAtStart !== benchmarkMutationEpoch) return;
  benchmarkState = state;
  renderBenchmarkState(state);
  if (state.status === "failed" && state.error && (reason === "initial" || reason === "manual" || reason === "tab")) setNotice(state.error, "error");
}

function requestBenchmarkRefresh(reason: RefreshReason = "poll"): Promise<void> {
  if (!benchmarkTabActive()) return Promise.resolve();
  if (benchmarkRefreshInFlight) {
    benchmarkRefreshQueued = true;
    return benchmarkRefreshInFlight;
  }
  const epoch = ++benchmarkRefreshEpoch;
  const task = performBenchmarkRefresh(epoch, reason).catch((error) => {
    if (benchmarkTabActive()) {
      const message = errorMessage(error);
      renderBenchmarkReadError(message);
      if (reason === "initial" || reason === "manual" || reason === "tab") {
        setNotice(message, "error");
        recordOperation("读取评测失败");
      }
    }
  });
  let tracked: Promise<void>;
  tracked = task.finally(() => {
    if (benchmarkRefreshInFlight === tracked) benchmarkRefreshInFlight = null;
    if (benchmarkRefreshQueued) {
      benchmarkRefreshQueued = false;
      if (benchmarkTabActive()) void requestBenchmarkRefresh("poll");
    }
  });
  benchmarkRefreshInFlight = tracked;
  return tracked;
}

function renderHistoryDetail(entry: InputData) {
  const detail = query<HTMLElement>("[data-history-detail]");
  const title = query<HTMLElement>("[data-history-detail-title]");
  const meta = query<HTMLElement>("[data-history-detail-meta]");
  const content = query<HTMLElement>("[data-history-detail-table]");
  if (!detail || !content) return;
  detail.classList.remove("is-hidden");
  if (title) title.textContent = "记录详情";
  const timestamp = formatTimestamp(entry.timestampMs);
  if (meta) meta.textContent = "上文：" + (entry.precedingText || "（空）") + " | 拼音：" + (entry.preedit || "—") + " | 时间：" + timestamp + " | 模型：" + (entry.model || "—");
  content.innerHTML = llmPerformanceSummary(entry.llmPerformance, entry.rimeMs, entry.endToEndMs) + diagnosticTable(entry);
  detail.scrollIntoView?.({ behavior: "smooth", block: "start" });
}

function closeHistoryDetail() {
  query<HTMLElement>("[data-history-detail]")?.classList.add("is-hidden");
  queueMicrotask(flushDeferredRenders);
}

async function fetchModelPresets(): Promise<ModelPreset[]> {
  return listModelPresets();
}

async function loadModelPresets(options: { force?: boolean } = {}) {
  const presets = await fetchModelPresets();
  currentPresets = presets;
  renderModelPresets(currentPresets, options);
}

function renderModelPresets(presets: ModelPreset[], options: { force?: boolean } = {}) {
  const key = JSON.stringify(presets);
  if (!options.force && key !== renderedPresetsKey && isFocusedWithin("[data-model-presets]")) {
    pendingPresets = presets;
    return;
  }
  pendingPresets = null;
  const count = query<HTMLElement>("[data-preset-count]");
  if (count) count.textContent = presets.length + " 个";
  const target = query<HTMLElement>("[data-model-presets]");
  if (!target) return;
  if (!options.force && key === renderedPresetsKey) return;
  if (!presets.length) {
    target.innerHTML = '<p class="muted">尚未保存模型预设。</p>';
    renderedPresetsKey = key;
    return;
  }
  target.innerHTML = presets.map((preset) => {
    const key = escapeHtml(preset.name);
    const loaded = preset.loaded ? " is-loaded" : "";
    const fullPath = preset.path || "—";
    return '<article class="preset-item' + loaded + '"><div class="preset-info"><strong>' + escapeHtml(preset.name) + '</strong><span class="muted mono" title="' + escapeHtml(fullPath) + '">' + escapeHtml(fullPath === "—" ? fullPath : truncatePath(fullPath, 56)) + '</span></div><div class="preset-actions"><button class="button button-primary" type="button" data-preset-action="select" data-preset-key="' + key + '">切换</button><button class="button" type="button" data-preset-action="rename" data-preset-key="' + key + '">重命名</button><button class="button button-danger" type="button" data-preset-action="delete" data-preset-key="' + key + '">删除</button></div></article>';
  }).join("");
  renderedPresetsKey = key;
}

async function performRefresh(reason: RefreshReason, mutationAtStart: number) {
  const historyAtStart = historyRefreshEpoch;
  const results = await Promise.allSettled([
    getConfig(),
    getStatus(),
    fetchDictionaryPage(1),
    fetchHistoryPage(currentHistoryPage),
    fetchModelPresets(),
  ]);

  // A user action or a newer refresh request may have completed while these IPC
  // calls were in flight. Never let that older snapshot overwrite the newer UI.
  if (mutationAtStart !== mutationEpoch) return;

  const errors: unknown[] = [];
  const configResult = results[0];
  const statusResult = results[1];
  const dictionaryResult = results[2];
  const historyResult = results[3];
  const presetsResult = results[4];

  let configSnapshot: ConfigSnapshot | null = null;
  if (configResult.status === "fulfilled") configSnapshot = configResult.value;
  else errors.push(configResult.reason);

  let statusConfig: ConfigSnapshot | null = null;
  if (statusResult.status === "fulfilled") {
    const rawStatus = statusResult.value;
    statusConfig = rawStatus.config;
    const fallbackStatusConfig = statusConfig ?? configSnapshot ?? currentStatus?.config ?? {
      revision: 0,
      config: { ...currentConfig },
    };
    const status: ServiceStatus = {
      state: rawStatus.state,
      config: fallbackStatusConfig,
      model: rawStatus.model,
    };
    currentStatus = status;
    renderStatus(status);
  } else {
    errors.push(statusResult.reason);
    currentStatus = null;
    renderStatus(null);
  }

  // Both endpoints can be in flight while another client saves settings. Use
  // the newest revision and keep the status snapshot on ties. If neither
  // endpoint returned a snapshot, leave the user's current form untouched.
  const effectiveConfig = configSnapshot && statusConfig
    ? (statusConfig.revision >= configSnapshot.revision ? statusConfig : configSnapshot)
    : configSnapshot ?? statusConfig;
  if (effectiveConfig) applyConfig(effectiveConfig);

  if (dictionaryResult.status === "fulfilled") {
    const dictionary = dictionaryResult.value as DictionaryPage;
    renderDictionary(dictionary.items, {}, dictionary.total);
  } else {
    errors.push(dictionaryResult.reason);
  }

  if (historyResult.status === "fulfilled") {
    let history = historyResult.value;
    const totalPages = Math.max(1, Math.ceil(history.total / HISTORY_PAGE_SIZE));
    if (history.page > totalPages) {
      currentHistoryPage = totalPages;
      try {
        history = await fetchHistoryPage(totalPages);
      } catch (error) {
        errors.push(error);
      }
      if (mutationAtStart !== mutationEpoch || historyAtStart !== historyRefreshEpoch) return;
    }
    if (mutationAtStart !== mutationEpoch || historyAtStart !== historyRefreshEpoch) return;
    renderHistory(history);
  } else errors.push(historyResult.reason);

  if (presetsResult.status === "fulfilled") {
    currentPresets = presetsResult.value;
    renderModelPresets(currentPresets);
  } else {
    errors.push(presetsResult.reason);
  }

  const reportError = reason === "initial" || reason === "manual";
  if (errors.length) {
    if (reportError) setNotice(errorMessage(errors[0]), "error");
    if (reason === "initial" || reason === "manual") recordOperation("刷新失败");
  } else if (reason === "initial" || reason === "manual") {
    setNotice("");
    recordOperation("已刷新");
  }
}

function requestRefresh(reason: RefreshReason = "poll"): Promise<void> {
  if (refreshInFlight) {
    refreshQueued = true;
    if (reason === "manual" || reason === "mutation" || queuedRefreshReason === null) queuedRefreshReason = reason;
    return refreshInFlight;
  }

  const mutationAtStart = mutationEpoch;
  const task = performRefresh(reason, mutationAtStart).catch((error) => {
    if (reason === "initial" || reason === "manual") {
      setNotice(errorMessage(error), "error");
      recordOperation("刷新失败");
    }
  });
  let tracked: Promise<void>;
  tracked = task.finally(() => {
    if (refreshInFlight === tracked) refreshInFlight = null;
    if (refreshQueued) {
      refreshQueued = false;
      const nextReason = queuedRefreshReason ?? "poll";
      queuedRefreshReason = null;
      void requestRefresh(nextReason);
    }
  });
  refreshInFlight = tracked;
  return tracked;
}

function refresh() {
  return requestRefresh("manual");
}

async function watchInputHistory() {
  // Keep one native long-poll active for the lifetime of the window. Unlike a
  // browser timer, the Tauri command continues waiting while the window is in
  // the background and resolves as soon as the service records or clears data.
  while (!historyWatchStopped) {
    try {
      const nextRevision = await waitForHistory(historyRevision);
      if (!Number.isFinite(nextRevision)) throw new Error("历史更新通知无效");
      if (nextRevision !== historyRevision) {
        historyRevision = nextRevision;
        void refreshHistoryNow();
      }
    } catch {
      await new Promise<void>((resolve) => window.setTimeout(resolve, HISTORY_WATCH_RETRY_MS));
    }
  }
}

async function refreshHistoryNow() {
  const mutationAtStart = mutationEpoch;
  const refreshAtStart = ++historyRefreshEpoch;
  try {
    let history = await fetchHistoryPage(currentHistoryPage);
    if (mutationAtStart !== mutationEpoch || refreshAtStart !== historyRefreshEpoch) return;
    const totalPages = Math.max(1, Math.ceil(history.total / HISTORY_PAGE_SIZE));
    if (history.page > totalPages) {
      currentHistoryPage = totalPages;
      history = await fetchHistoryPage(totalPages);
      if (mutationAtStart !== mutationEpoch || refreshAtStart !== historyRefreshEpoch) return;
    }
    renderHistory(history);
  } catch {
    // The regular management refresh will report service errors. History
    // notifications retry through the long-poll loop without surfacing a
    // transient connection failure as a toast.
  }
}

function flushDeferredRenders() {
  if (pendingConfigSnapshot && !configFormDirty && !isFocusedWithin("[data-config-form]")) {
    const snapshot = pendingConfigSnapshot;
    pendingConfigSnapshot = null;
    applyConfig(snapshot);
  }
  if (pendingPresets && !isFocusedWithin("[data-model-presets]")) {
    const presets = pendingPresets;
    pendingPresets = null;
    renderModelPresets(presets, { force: true });
  }
}

const configForm = query<HTMLFormElement>("[data-config-form]");
configForm?.addEventListener("input", () => { configFormDirty = true; });
configForm?.addEventListener("change", () => { configFormDirty = true; });
query<HTMLSelectElement>("[data-theme-mode]")?.addEventListener("change", (event) => {
  applyThemeMode((event.target as HTMLSelectElement).value);
});

configForm?.addEventListener("submit", async (event) => {
  event.preventDefault();
  markMutation();
  const previousConfig = { ...currentConfig };
  const nextConfig = readConfig();
  const activeModelPath = currentStatus?.model.loaded ? currentStatus.model.path : null;
  const reloadModel = Boolean(activeModelPath && (
    previousConfig.llm_context_token_limit !== nextConfig.llm_context_token_limit
    || previousConfig.llm_rerank_count !== nextConfig.llm_rerank_count
    || previousConfig.llm_inference_count_limit !== nextConfig.llm_inference_count_limit
    || previousConfig.llm_backend !== nextConfig.llm_backend
  ));
  try {
    const snapshot = await setConfig(nextConfig);
    applyConfig(snapshot, { force: true });
    if (currentStatus) currentStatus.config = snapshot;
    renderStatus(currentStatus);
    applyThemeMode(query<HTMLSelectElement>("[data-theme-mode]")?.value, true);
    if (reloadModel && activeModelPath) {
      try {
        const model = await loadModel(activeModelPath);
        renderModel(model);
        if (currentStatus) currentStatus.model = model;
        await loadModelPresets({ force: true });
        setNotice("设置已保存，模型已重新加载", "success");
        recordOperation("设置已保存，模型已重新加载");
      } catch (error) {
        setNotice("设置已保存，但模型重新加载失败：" + errorMessage(error), "error");
        recordOperation("设置已保存，模型重新加载失败");
      }
    } else {
      setNotice("设置已保存", "success");
      recordOperation("设置已保存");
    }
    void requestRefresh("mutation");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("保存设置失败");
    void requestRefresh("mutation");
  }
});

query<HTMLFormElement>("[data-model-form]")?.addEventListener("submit", async (event) => {
  event.preventDefault();
  const input = query<HTMLInputElement>("[data-model-path-input]");
  const path = input?.value.trim() ?? "";
  if (!path) return setNotice("请输入 GGUF 文件路径", "error");
  markMutation();
  try {
    const model = await loadModel(path);
    renderModel(model);
    if (currentStatus) currentStatus.model = model;
    await loadModelPresets({ force: true });
    setNotice("模型已加载", "success");
    recordOperation("模型已加载");
    void requestRefresh("mutation");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("加载模型失败");
    void requestRefresh("mutation");
  }
});

query<HTMLButtonElement>("[data-unload-model]")?.addEventListener("click", async () => {
  markMutation();
  try {
    const model = await unloadModel();
    renderModel(model);
    if (currentStatus) currentStatus.model = model;
    await loadModelPresets({ force: true });
    setNotice("模型已卸载，当前使用基础模式", "success");
    recordOperation("模型已卸载");
    void requestRefresh("mutation");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("卸载模型失败");
    void requestRefresh("mutation");
  }
});

const presetForm = query<HTMLFormElement>("[data-preset-form]");
const addModelButton = query<HTMLButtonElement>("[data-add-model]");
const cancelAddModelButton = query<HTMLButtonElement>("[data-cancel-add-model]");
function setPresetFormVisible(visible: boolean) {
  presetForm?.classList.toggle("is-hidden", !visible);
  if (addModelButton) {
    addModelButton.setAttribute("aria-expanded", String(visible));
    addModelButton.textContent = visible ? "收起" : "添加模型";
  }
  if (visible) query<HTMLInputElement>("[data-preset-name]")?.focus();
}
addModelButton?.addEventListener("click", () => setPresetFormVisible(Boolean(presetForm?.classList.contains("is-hidden"))));
cancelAddModelButton?.addEventListener("click", () => {
  presetForm?.reset();
  setPresetFormVisible(false);
});

query<HTMLFormElement>("[data-preset-form]")?.addEventListener("submit", async (event) => {
  event.preventDefault();
  const nameInput = query<HTMLInputElement>("[data-preset-name]");
  const pathInput = query<HTMLInputElement>("[data-preset-path]");
  const name = nameInput?.value.trim() ?? "";
  const path = pathInput?.value.trim() ?? "";
  if (!name || !path) return setNotice("请输入预设名称和 GGUF 路径", "error");
  markMutation();
  try {
    await saveModelPreset(name, path);
    await loadModelPresets({ force: true });
    presetForm?.reset();
    setPresetFormVisible(false);
    setNotice("模型预设已保存", "success");
    recordOperation("模型预设已保存");
    void requestRefresh("mutation");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("保存模型预设失败");
    void requestRefresh("mutation");
  }
});

query<HTMLElement>("[data-model-presets]")?.addEventListener("click", async (event) => {
  const target = event.target as HTMLElement;
  const button = target.closest<HTMLButtonElement>("[data-preset-action]");
  if (!button) return;
  const key = button.dataset.presetKey ?? "";
  const preset = currentPresets.find((item) => item.name === key);
  if (!preset) return;
  const action = button.dataset.presetAction;
  markMutation();
  try {
    if (action === "select") {
      // The Tauri command and protocol identify a preset by its persisted name.
      // `id` is only a UI-derived alias; sending it first makes Tauri reject the
      // request before the canonical `{ name }` payload is attempted, and the
      // compatibility helper would then surface that misleading first error.
      const presetResult = await selectModelPreset(preset.name);
      const model = {
        path: presetResult.path,
        sizeBytes: presetResult.sizeBytes,
        sha256: presetResult.sha256,
        loaded: presetResult.loaded,
        scoringPath: null,
        memory: {},
      } satisfies ModelInfo;
      if (model.path !== null || model.loaded) {
        renderModel(model);
        if (currentStatus) currentStatus.model = model;
      }
      await loadModelPresets({ force: true });
      setNotice("已切换到预设：" + preset.name, "success");
      recordOperation("模型预设已切换");
    } else if (action === "rename") {
      const newName = window.prompt("请输入新的模型预设名称", preset.name)?.trim();
      if (!newName || newName === preset.name) return;
      await renameModelPreset(preset.name, newName);
      await loadModelPresets({ force: true });
      setNotice("模型预设已重命名", "success");
      recordOperation("模型预设已重命名");
    } else if (action === "delete") {
      if (!window.confirm("确定删除模型预设“" + preset.name + "”吗？")) return;
      await deleteModelPreset(preset.name);
      await loadModelPresets({ force: true });
      setNotice("模型预设已删除", "success");
      recordOperation("模型预设已删除");
    }
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("模型预设操作失败");
  } finally {
    void requestRefresh("mutation");
  }
});

query<HTMLButtonElement>("[data-import-dictionary]")?.addEventListener("click", () => query<HTMLInputElement>("[data-dictionary-file]")?.click());
query<HTMLInputElement>("[data-dictionary-file]")?.addEventListener("change", async (event) => {
  const file = (event.target as HTMLInputElement).files?.[0];
  if (!file) return;
  try {
    const parsed: unknown = JSON.parse(await file.text());
    if (!Array.isArray(parsed)) throw new Error("词库 JSON 必须是条目数组");
    const entries = parsed.map(validateEntry);
    markMutation();
    await importDictionary(entries);
    const updated = await fetchDictionaryPage(1);
    renderDictionary(updated.items, {}, updated.total);
    setNotice("已导入 " + entries.length + " 条词库记录", "success");
    recordOperation("词库已导入");
    void requestRefresh("mutation");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("导入词库失败");
    void requestRefresh("mutation");
  } finally {
    (event.target as HTMLInputElement).value = "";
  }
});

query<HTMLButtonElement>("[data-export-dictionary]")?.addEventListener("click", async () => {
  try {
    const entries = await fetchAllDictionaryEntries();
    const blob = new Blob([JSON.stringify(entries, null, 2)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = "lime-dictionary.json";
    link.click();
    URL.revokeObjectURL(url);
    setNotice("已导出 " + entries.length + " 条词库记录", "success");
    recordOperation("词库已导出");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("导出词库失败");
  }
});

query<HTMLButtonElement>("[data-clear-dictionary]")?.addEventListener("click", async () => {
  if (!window.confirm("确定清空用户词库吗？此操作不可撤销。")) return;
  markMutation();
  try {
    await clearDictionary();
    renderDictionary([], { force: true });
    setNotice("用户词库已清空", "success");
    recordOperation("词库已清空");
    void requestRefresh("mutation");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("清空词库失败");
    void requestRefresh("mutation");
  }
});

query<HTMLFormElement>("[data-test-form]")?.addEventListener("submit", async (event) => {
  event.preventDefault();
  const precedingText = query<HTMLTextAreaElement>("[data-test-context]")?.value ?? "";
  const preedit = query<HTMLInputElement>("[data-test-preedit]")?.value.trim() ?? "";
  if (!preedit) return setNotice("请输入拼音", "error");
  markMutation();
  try {
    const response = await testInput(precedingText, preedit);
    // A successful test creates the newest history entry.  Return to page one so the
    // just-completed request is immediately visible in the newest-first list.
    currentHistoryPage = 1;
    const history = await fetchHistoryPage(currentHistoryPage);
    renderHistory(history);
    renderTestResult(response);
    setNotice("候选请求已完成", "success");
    recordOperation("输入测试已完成");
    void requestRefresh("mutation");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("输入测试失败");
    void requestRefresh("mutation");
  }
});

query<HTMLButtonElement>("[data-test-clear]")?.addEventListener("click", () => {
  const context = query<HTMLTextAreaElement>("[data-test-context]");
  const preedit = query<HTMLInputElement>("[data-test-preedit]");
  if (context) context.value = "";
  if (preedit) preedit.value = "";
  const result = query<HTMLElement>("[data-test-result]");
  if (result) result.innerHTML = "";
  const request = query<HTMLElement>("[data-test-request]");
  if (request) request.textContent = "尚未请求";
});

function selectedBenchmarkModes(): BenchmarkMode[] {
  return all<HTMLInputElement>("[data-benchmark-mode]")
    .filter((input) => input.checked)
    .map((input) => input.value)
    .filter((value): value is BenchmarkMode => value === "full" || value === "initials");
}

function selectedBenchmarkCategories(): string[] {
  return all<HTMLInputElement>("[data-benchmark-category]")
    .filter((input) => input.checked)
    .map((input) => input.value);
}

query<HTMLFormElement>("[data-benchmark-form]")?.addEventListener("submit", async (event) => {
  event.preventDefault();
  const modes = selectedBenchmarkModes();
  const categories = selectedBenchmarkCategories();
  if (!modes.length) return setNotice("至少选择一种拼音模式", "error");
  if (!categories.length) return setNotice("至少选择一类语料", "error");
  const actionEpoch = ++benchmarkMutationEpoch;
  markMutation();
  try {
    const state = await startBenchmark({ modes, categories });
    if (actionEpoch !== benchmarkMutationEpoch) return;
    benchmarkState = state;
    benchmarkRenderedReportKey = "";
    clearBenchmarkReportView();
    renderBenchmarkState(state);
    setNotice("评测已开始", "success");
    recordOperation("评测已开始");
    void requestBenchmarkRefresh("mutation");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("启动评测失败");
  }
});

query<HTMLButtonElement>("[data-benchmark-stop]")?.addEventListener("click", async () => {
  if (benchmarkState.status !== "running") return;
  const actionEpoch = ++benchmarkMutationEpoch;
  markMutation();
  try {
    await stopBenchmark();
    if (actionEpoch !== benchmarkMutationEpoch) return;
    benchmarkState = { ...benchmarkState, status: "stopping" };
    renderBenchmarkState(benchmarkState);
    setNotice("正在停止评测", "info");
    recordOperation("正在停止评测");
    void requestBenchmarkRefresh("mutation");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("停止评测失败");
  }
});

query<HTMLButtonElement>("[data-benchmark-export]")?.addEventListener("click", () => {
  if (!benchmarkState.report) return;
  const payload = {
    dataset: benchmarkDataset,
    report: benchmarkState.report,
    status: benchmarkState.status,
    model: benchmarkState.modelName,
    modelSha256: benchmarkState.modelSha256,
    config: benchmarkState.config,
    configRevision: benchmarkState.configRevision,
  };
  const blob = new Blob([JSON.stringify(payload, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = "lime-benchmark.json";
  link.click();
  URL.revokeObjectURL(url);
  setNotice("评测报告已导出", "success");
  recordOperation("评测报告已导出");
});

query<HTMLTableSectionElement>("[data-history-table]")?.addEventListener("click", (event) => {
  const target = event.target as HTMLElement;
  const row = target.closest<HTMLElement>("[data-history-index]");
  if (!row) return;
  const index = Number(row.dataset.historyIndex);
  const entry = currentHistory[index];
  if (entry) renderHistoryDetail(entry);
});

query<HTMLTableSectionElement>("[data-history-table]")?.addEventListener("keydown", (event) => {
  if (event.key !== "Enter" && event.key !== " ") return;
  const target = event.target as HTMLElement;
  const row = target.closest<HTMLElement>("[data-history-index]");
  if (!row) return;
  event.preventDefault();
  const index = Number(row.dataset.historyIndex);
  const entry = currentHistory[index];
  if (entry) renderHistoryDetail(entry);
});

query<HTMLButtonElement>("[data-history-detail-close]")?.addEventListener("click", closeHistoryDetail);
query<HTMLButtonElement>("[data-history-prev]")?.addEventListener("click", async () => {
  if (currentHistoryPage <= 1) return;
  markMutation();
  try {
    renderHistory(await fetchHistoryPage(currentHistoryPage - 1));
    closeHistoryDetail();
  } catch (error) {
    setNotice(errorMessage(error), "error");
  } finally {
    void requestRefresh("mutation");
  }
});
query<HTMLButtonElement>("[data-history-next]")?.addEventListener("click", async () => {
  const totalPages = Math.max(1, Math.ceil(currentHistoryTotal / HISTORY_PAGE_SIZE));
  if (currentHistoryPage >= totalPages) return;
  markMutation();
  try {
    renderHistory(await fetchHistoryPage(currentHistoryPage + 1));
    closeHistoryDetail();
  } catch (error) {
    setNotice(errorMessage(error), "error");
  } finally {
    void requestRefresh("mutation");
  }
});

query<HTMLButtonElement>("[data-clear-history]")?.addEventListener("click", async () => {
  if (!window.confirm("确定清空输入历史吗？")) return;
  markMutation();
  try {
    await clearHistory();
    currentHistoryPage = 1;
    renderHistory({ items: [], total: 0, page: 1, pageSize: HISTORY_PAGE_SIZE }, { force: true });
    closeHistoryDetail();
    setNotice("输入历史已清空", "success");
    recordOperation("输入历史已清空");
    void requestRefresh("mutation");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("清空输入历史失败");
    void requestRefresh("mutation");
  }
});

query<HTMLButtonElement>("[data-refresh]")?.addEventListener("click", () => { void refresh(); });
query<HTMLButtonElement>("[data-notice-close]")?.addEventListener("click", () => setNotice(""));
document.addEventListener("focusout", () => { queueMicrotask(flushDeferredRenders); });
for (const tab of all<HTMLButtonElement>("[data-tab]")) {
  tab.addEventListener("click", () => {
    const name = tab.dataset.tab;
    if (!name) return;
    query<HTMLElement>(".shell")?.setAttribute("data-active-tab", name);
    for (const item of all<HTMLButtonElement>("[data-tab]")) item.classList.toggle("is-active", item === tab);
    for (const panel of all<HTMLElement>("[data-panel]")) panel.classList.toggle("is-hidden", panel.dataset.panel !== name);
    flushDeferredRenders();
    void requestRefresh("tab");
    if (name === "benchmark") void requestBenchmarkRefresh("tab");
  });
}

function validateEntry(value: unknown): DictionaryEntry {
  if (!value || typeof value !== "object") throw new Error("词库条目格式无效");
  const entry = value as Partial<DictionaryEntry>;
  if (typeof entry.pinyin !== "string" || !entry.pinyin.trim() || typeof entry.text !== "string" || !entry.text.trim() || typeof entry.weight !== "number" || !Number.isInteger(entry.weight)) {
    throw new Error("词库条目必须包含有效的 pinyin、text 和整数 weight");
  }
  return { pinyin: entry.pinyin, text: entry.text, weight: entry.weight };
}

window.addEventListener("beforeunload", () => { historyWatchStopped = true; });
document.addEventListener("visibilitychange", () => {
  void requestRefresh("visibility");
  if (benchmarkTabActive()) void requestBenchmarkRefresh("visibility");
});

window.setInterval(() => {
  void requestRefresh("poll");
  if (benchmarkTabActive()) void requestBenchmarkRefresh("poll");
}, REFRESH_INTERVAL_MS);

void requestRefresh("initial");
void watchInputHistory();
