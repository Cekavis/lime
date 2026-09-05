import { invoke } from "@tauri-apps/api/core";
import "./style.css";

type ServiceState = "ready" | "rime_only" | "reloading" | "unavailable";

interface Config {
  rime_schema: string;
  preceding_text_char_limit: number;
  context_preview_char_limit: number;
  page_size: number;
  llm_rerank_count: number;
  llm_effective_count: number;
  llm_context_token_limit: number;
  llm_backend: "cuda" | "cpu";
}

interface ConfigSnapshot {
  revision: number;
  config: Config;
}

interface ModelInfo {
  path: string | null;
  size_bytes: number | null;
  sha256: string | null;
  loaded: boolean;
  memory: Record<string, number>;
}

interface ModelPreset {
  id: string;
  name: string;
  path: string | null;
  sizeBytes: number | null;
  sha256: string | null;
  loaded: boolean;
}

interface Candidate {
  displayText: string;
  commitText: string;
}

interface CandidateDiagnostic {
  index: number;
  rimeCandidate: Candidate | null;
  llmCandidate: Candidate | null;
  logprob: number | null;
  logprobs: number[];
  mismatch: boolean | null;
  displayCandidate: Candidate | null;
  hasDisplayCandidate: boolean;
}

interface LlmPerformance {
  totalMs: number;
  tokenizeMs: number;
  decodeMs: number;
  logitsMs: number;
  rimeMs: number | null;
  candidateCount: number;
  scoredCount: number;
  targetTokenCount: number;
  batchCount: number;
  mismatchCount: number;
  contextTokenCount: number;
  decodeInputTokenCount: number;
  logitsOutputCount: number;
}

interface InputData {
  requestId?: number;
  timestampMs: number | null;
  precedingText: string;
  preedit: string;
  rimeCandidates: Candidate[];
  finalCandidates: Candidate[];
  diagnostics: CandidateDiagnostic[];
  llmPerformance: LlmPerformance | null;
  rimeMs: number | null;
  model: string | null;
  contextUsed: boolean | null;
  serviceState: ServiceState;
}

interface HistoryPage {
  items: InputData[];
  total: number;
  page: number;
  pageSize: number;
}

interface ServiceStatus {
  state: ServiceState;
  config: ConfigSnapshot;
  model: ModelInfo;
}

interface DictionaryEntry {
  pinyin: string;
  text: string;
  weight: number;
}

interface DictionaryPage {
  items: DictionaryEntry[];
  total: number;
  page: number;
  pageSize: number;
}

type RefreshReason = "initial" | "manual" | "poll" | "tab" | "visibility" | "mutation";

const REFRESH_INTERVAL_MS = 3000;
const HISTORY_WATCH_RETRY_MS = 1000;
const DICTIONARY_PAGE_SIZE = 100;

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
  llm_backend: "cuda",
};

type ThemeMode = "system" | "light" | "dark";
const THEME_STORAGE_KEY = "lime.management.theme";

const HISTORY_PAGE_SIZE = 100;
const NOTICE_DURATION_MS = 4000;
const app = document.querySelector<HTMLDivElement>("#app");
if (!app) throw new Error("Lime UI mount point is missing");

app.innerHTML = [
  '<div class="shell">',
  '  <header class="header">',
  '    <div class="brand" aria-label="Lime"><img class="brand-logo" src="/logo.svg" alt="Lime" /></div>',
  '    <nav class="tabs" aria-label="Lime 功能">',
  '      <button class="tab is-active" type="button" data-tab="input">设置</button>',
  '      <button class="tab" type="button" data-tab="test">测试</button>',
  '      <button class="tab" type="button" data-tab="dictionary">词库</button>',
  '      <button class="tab" type="button" data-tab="history">历史</button>',
  '      <button class="tab" type="button" data-tab="diagnostics">诊断</button>',
  '    </nav>',
  '    <span class="badge" data-service-state="unavailable" aria-live="polite">服务 不可用</span>',
  "  </header>",
  '  <div class="toast-region" data-notice-region aria-live="polite" aria-atomic="true">',
  '    <div class="notice is-hidden" data-notice role="status" aria-hidden="true">',
  '      <span data-notice-message></span>',
  '      <button class="toast-close" data-notice-close type="button" aria-label="关闭提示">×</button>',
  "    </div>",
  "  </div>",
  "  <main>",
  '    <section class="panel" data-panel="input">',
  '      <div class="panel-heading"><h2>设置</h2></div>',
  '      <form data-config-form>',
  '        <section class="settings-section settings-section-first">',
  '          <div class="form-grid">',
  '            <label class="field"><span>每页展示候选词数</span><input data-config="page_size" type="number" min="1" max="20" required /></label>',
  '            <label class="field"><span>前文预览字符数</span><input data-config="context_preview_char_limit" type="number" min="0" max="1024" required /></label>',
  '            <label class="field"><span>前文实际获取字符数</span><input data-config="preceding_text_char_limit" type="number" min="1" max="4096" required /></label>',
  '            <label class="field"><span>管理界面深色模式</span><select data-theme-mode><option value="system">跟随系统</option><option value="light">固定浅色</option><option value="dark">固定深色</option></select></label>',
  '          </div>',
  '        </section>',
  '        <section class="settings-section">',
  '          <div class="form-grid">',
  '            <label class="field"><span>Rime 方案</span><select data-config="rime_schema"><option value="rime_ice">雾凇拼音（全拼）</option><option value="double_pinyin">自然码双拼</option><option value="double_pinyin_abc">智能 ABC 双拼</option><option value="double_pinyin_mspy">微软双拼</option><option value="double_pinyin_sogou">搜狗双拼</option><option value="double_pinyin_flypy">小鹤双拼</option><option value="double_pinyin_ziguang">紫光双拼</option><option value="double_pinyin_jiajia">拼音加加双拼</option></select></label>',
  '            <label class="field"><span>LLM 单次请求 token 数上限</span><input data-config="llm_context_token_limit" type="number" min="1" max="4096" required /></label>',
  '            <label class="field"><span>LLM 重排输入候选词数</span><input data-config="llm_rerank_count" type="number" min="1" max="128" required /></label>',
  '            <label class="field"><span>LLM 重排采纳候选词数</span><input data-config="llm_effective_count" type="number" min="1" max="32" required /></label>',
  '            <label class="field"><span>LLM 后端</span><select data-config="llm_backend"><option value="cuda">CUDA</option><option value="cpu">CPU</option></select></label>',
  '          </div>',
  '        </section>',
  '        <div class="actions settings-actions"><button class="button button-primary" type="submit">保存设置</button></div>',
  '      </form>',
  '      <section class="settings-section model-status-section"><div class="section-heading"><span class="status-dot" data-model-state>未加载</span></div>',
  '        <div class="model-card"><dl class="status-list"><dt>路径</dt><dd class="model-path" data-model-path title="—">—</dd><dt>文件大小</dt><dd data-model-size>—</dd></dl><div class="model-memory" data-model-memory><p class="muted">未加载模型，暂无显存占用信息。</p></div></div>',
  '      </section>',
  '      <section class="settings-section preset-section"><div class="section-heading"><div class="section-heading-actions"><span class="meta-badge" data-preset-count>0 个</span><button class="button button-danger" data-unload-model type="button">卸载模型</button></div></div>',
  '        <div class="actions model-add-actions"><button class="button" data-add-model type="button" aria-expanded="false">添加模型</button></div>',
  '        <form class="preset-form is-hidden" data-preset-form><label class="field"><span>名称</span><input data-preset-name type="text" placeholder="例如 Qwen 7B" required /></label><label class="field field-wide"><span>GGUF 文件路径</span><input data-preset-path type="text" placeholder="C:\\Models\\lime.gguf" required /></label><div class="actions"><button class="button button-primary" type="submit">保存模型</button><button class="button" data-cancel-add-model type="button">取消</button></div></form>',
  '        <div class="preset-list" data-model-presets><p class="muted">尚未读取模型预设。</p></div>',
  '      </section>',
  '      <form class="model-form is-hidden" data-model-form><input data-model-path-input type="text" aria-hidden="true" tabindex="-1" /></form>',
  "    </section>",
  '    <section class="panel is-hidden" data-panel="test">',
  '      <div class="panel-heading"><div><h2>输入测试</h2></div><span class="meta-badge" data-test-request>尚未请求</span></div>',
  '      <form data-test-form>',
  '        <label class="field field-wide field-stacked"><span>上文</span><textarea data-test-context rows="3" placeholder="可选：输入光标前的中文文本"></textarea></label>',
  '        <label class="field field-wide"><span>拼音</span><input data-test-preedit type="text" placeholder="例如 nihao" required /></label>',
  '        <div class="actions"><button class="button button-primary" type="submit">请求候选</button><button class="button" data-test-clear type="button">清空结果</button></div>',
  "      </form>",
  '      <div class="test-result" data-test-result><p class="muted">输入上文和拼音后查看服务结果。</p></div>',
  "    </section>",
  '    <section class="panel is-hidden" data-panel="dictionary">',
  '      <div class="panel-heading"><h2>词库</h2><span class="meta-badge" data-dictionary-count>— 条</span></div>',
  '      <div class="actions"><button class="button" data-import-dictionary type="button">导入 JSON</button><button class="button" data-export-dictionary type="button">导出 JSON</button><button class="button button-danger" data-clear-dictionary type="button">清空用户词库</button><input class="visually-hidden" data-dictionary-file type="file" accept="application/json,.json" /></div>',
  '      <div class="table-wrap"><table><thead><tr><th>拼音</th><th>文本</th><th>权重</th></tr></thead><tbody data-dictionary-table><tr><td colspan="3" class="muted">尚未读取词库</td></tr></tbody></table></div>',
  "    </section>",
  '    <section class="panel is-hidden" data-panel="history">',
  '      <div class="panel-heading"><h2>历史</h2><div class="actions-inline"><span class="meta-badge" data-history-count>0 条</span><button class="button button-danger" data-clear-history type="button">清空历史</button></div></div>',
  '      <div class="table-wrap"><table class="history-table"><thead><tr><th>上文</th><th>拼音</th><th>Rime 候选（前 2）</th><th>LLM 排序（前 2）</th><th>LLM 总耗时</th><th>模型</th></tr></thead><tbody data-history-table><tr><td colspan="6" class="muted">暂无输入记录</td></tr></tbody></table></div>',
  '      <div class="pagination" data-history-pagination><button class="button" data-history-prev type="button">上一页</button><span data-history-page-label>第 1 页</span><button class="button" data-history-next type="button">下一页</button></div>',
  '      <section class="history-detail is-hidden" data-history-detail aria-live="polite"><div class="panel-heading compact-heading"><p class="muted" data-history-detail-meta>—</p><button class="button" data-history-detail-close type="button">返回列表</button></div><div data-history-detail-table><p class="muted">选择一条记录查看详情。</p></div></section>',
  "    </section>",
  '    <section class="panel is-hidden" data-panel="diagnostics">',
  '      <div class="panel-heading"><h2>诊断</h2><button class="button" data-refresh type="button">刷新</button></div>',
  '      <dl class="status-list diagnostics-list"><dt>服务状态</dt><dd data-diagnostic-state>—</dd><dt>模型</dt><dd data-diagnostic-model>—</dd><dt>词库条目</dt><dd data-diagnostic-dictionary>—</dd><dt>最近操作</dt><dd data-last-operation>—</dd></dl>',
  "    </section>",
  "  </main>",
  "</div>",
].join("\n");

let currentConfig: Config = { ...defaultConfig };
let currentStatus: ServiceStatus | null = null;
let currentHistory: InputData[] = [];
let currentHistoryPage = 1;
let currentHistoryTotal = 0;
let currentPresets: ModelPreset[] = [];

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

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : null;
}

function firstValue(record: Record<string, unknown> | null, keys: string[]): unknown {
  if (!record) return undefined;
  for (const key of keys) if (record[key] !== undefined && record[key] !== null) return record[key];
  return undefined;
}

function fieldValue(record: Record<string, unknown> | null, keys: string[]): { present: boolean; value: unknown } {
  if (!record) return { present: false, value: undefined };
  for (const key of keys) {
    if (Object.prototype.hasOwnProperty.call(record, key)) return { present: true, value: record[key] };
  }
  return { present: false, value: undefined };
}

function asString(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : value == null ? fallback : String(value);
}

function asNumber(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string" && value.trim() && Number.isFinite(Number(value))) return Number(value);
  return null;
}

function asBoolean(value: unknown): boolean | null {
  return typeof value === "boolean" ? value : null;
}

function normalizeState(value: unknown): ServiceState {
  return value === "ready" || value === "rime_only" || value === "reloading" || value === "unavailable" ? value : "unavailable";
}

function normalizeCandidate(value: unknown): Candidate | null {
  if (typeof value === "string") return { displayText: value, commitText: value };
  const record = asRecord(value);
  if (!record) return null;
  const display = asString(firstValue(record, ["display_text", "displayText", "text", "commit_text", "commitText"]));
  const commit = asString(firstValue(record, ["commit_text", "commitText", "display_text", "displayText", "text"]), display);
  return display || commit ? { displayText: display || commit, commitText: commit || display } : null;
}

function normalizeCandidates(value: unknown): Candidate[] {
  if (!Array.isArray(value)) return [];
  return value.map(normalizeCandidate).filter((candidate): candidate is Candidate => candidate !== null);
}

function normalizeLogprobs(value: unknown): number[] {
  if (!Array.isArray(value)) return [];
  return value.map(asNumber).filter((item): item is number => item !== null);
}

function normalizeLlmPerformance(value: unknown): LlmPerformance | null {
  const record = asRecord(value);
  if (!record) return null;
  const totalMs = asNumber(firstValue(record, ["total_ms", "totalMs", "total_duration_ms", "totalDurationMs"]));
  if (totalMs == null) return null;
  const count = (keys: string[]) => Math.max(0, Math.trunc(asNumber(firstValue(record, keys)) ?? 0));
  return {
    totalMs: Math.max(0, totalMs),
    tokenizeMs: Math.max(0, asNumber(firstValue(record, ["tokenize_ms", "tokenizeMs"])) ?? 0),
    decodeMs: Math.max(0, asNumber(firstValue(record, ["decode_ms", "decodeMs"])) ?? 0),
    logitsMs: Math.max(0, asNumber(firstValue(record, ["logits_ms", "logitsMs"])) ?? 0),
    rimeMs: (() => {
      const value = asNumber(firstValue(record, ["rime_duration_ms", "rimeDurationMs", "rime_ms", "rimeMs"]));
      return value == null ? null : Math.max(0, value);
    })(),
    candidateCount: count(["candidate_count", "candidateCount"]),
    scoredCount: count(["scored_count", "scoredCount"]),
    targetTokenCount: count(["target_token_count", "targetTokenCount"]),
    batchCount: count(["batch_count", "batchCount"]),
    mismatchCount: count(["mismatch_count", "mismatchCount"]),
    contextTokenCount: count(["context_token_count", "contextTokenCount"]),
    decodeInputTokenCount: count(["decode_input_token_count", "decodeInputTokenCount"]),
    logitsOutputCount: count(["logits_output_count", "logitsOutputCount"]),
  };
}

function normalizeDiagnostic(value: unknown, index: number, rime: Candidate[], final: Candidate[]): CandidateDiagnostic {
  const record = asRecord(value);
  const rimeField = fieldValue(record, ["rime_candidate", "rimeCandidate", "rime", "raw_candidate"]);
  const llmField = fieldValue(record, ["llm_candidate", "llmCandidate", "candidate", "final_candidate"]);
  const displayField = fieldValue(record, ["display_candidate", "displayCandidate", "shown_candidate", "shownCandidate"]);
  const explicitRime = normalizeCandidate(rimeField.value);
  const explicitLlm = normalizeCandidate(llmField.value);
  const rawIndex = asNumber(firstValue(record, ["index"]));
  const rawRank = asNumber(firstValue(record, ["rank", "position"]));
  const diagnosticIndex = rawRank == null
    ? rawIndex == null ? index : Math.max(0, Math.trunc(rawIndex))
    : Math.max(0, Math.trunc(rawRank) - 1);
  const rimeCandidate = rimeField.present ? explicitRime : rime[diagnosticIndex] ?? null;
  const llmCandidate = llmField.present ? explicitLlm : final[diagnosticIndex] ?? null;
  const displayCandidate = normalizeCandidate(displayField.value);
  const logprob = asNumber(firstValue(record, ["logprob", "score", "llm_logprob"]));
  const mismatch = asBoolean(firstValue(record, ["mismatch", "token_mismatch"]));
  return {
    index: diagnosticIndex,
    rimeCandidate,
    llmCandidate,
    logprob,
    logprobs: normalizeLogprobs(firstValue(record, ["logprobs", "token_logprobs", "tokenLogprobs"])),
    mismatch,
    displayCandidate,
    hasDisplayCandidate: displayField.present,
  };
}

function diagnosticsFrom(raw: unknown, rime: Candidate[], final: Candidate[]): CandidateDiagnostic[] {
  const values = Array.isArray(raw) ? raw : [];
  const count = Math.max(values.length, rime.length, final.length);
  const result = Array.from({ length: count }, (_, index) => normalizeDiagnostic(values[index], index, rime, final));
  const hasScore = (diagnostic: CandidateDiagnostic) =>
    diagnostic.logprob != null && diagnostic.logprobs.length > 0;
  return result.sort((left, right) => {
    // The wire format uses logprob=0 for rows outside llm_rerank_count. Their empty
    // logprobs array is the authoritative marker that they were not scored. Without this
    // check, all untouched Rime rows sort ahead of the first LLM-ranked candidates.
    const leftScored = hasScore(left);
    const rightScored = hasScore(right);
    if (!leftScored && !rightScored) return left.index - right.index;
    if (!leftScored) return 1;
    if (!rightScored) return -1;
    return Math.abs(left.logprob!) - Math.abs(right.logprob!) || left.index - right.index;
  });
}

function normalizeInputData(value: unknown, fallback: Partial<InputData> = {}): InputData {
  const record = asRecord(value);
  const rawRime = firstValue(record, ["rime_candidates", "rimeCandidates", "raw_candidates", "rawCandidates"]);
  const rawFinal = firstValue(record, ["final_candidates", "finalCandidates", "candidates", "llm_candidates", "llmCandidates"]);
  const rime = Array.isArray(rawRime) ? normalizeCandidates(rawRime) : fallback.rimeCandidates || [];
  const final = Array.isArray(rawFinal) ? normalizeCandidates(rawFinal) : fallback.finalCandidates || [];
  const diagnosticsRaw = firstValue(record, ["diagnostics", "candidate_diagnostics", "candidateDiagnostics"]);
  const performanceRaw = firstValue(record, ["llm_performance", "llmPerformance", "performance"]);
  const performanceRecord = asRecord(performanceRaw);
  const rimeDuration = asNumber(firstValue(record, [
    "rime_duration_ms", "rimeDurationMs", "rime_ms", "rimeMs", "rime_elapsed_ms", "rimeElapsedMs",
  ])) ?? asNumber(firstValue(performanceRecord, ["rime_duration_ms", "rimeDurationMs", "rime_ms", "rimeMs"]));
  const modelValue = firstValue(record, ["model_name", "modelName", "model", "model_id", "modelId"]);
  const modelRecord = asRecord(modelValue);
  const model = asString(
    modelRecord
      ? firstValue(modelRecord, ["name", "model_name", "modelName", "path", "model_path", "modelPath"])
      : modelValue,
    "",
  ) || null;
  const precedingText = asString(firstValue(record, ["preceding_text", "precedingText", "context"]), fallback.precedingText || "");
  const preedit = asString(firstValue(record, ["preedit", "input_preedit", "inputPreedit"]), fallback.preedit || "");
  const timestamp = asNumber(firstValue(record, ["timestamp_ms", "timestampMs", "created_at_ms", "createdAtMs", "timestamp"]));
  return {
    requestId: asNumber(firstValue(record, ["request_id", "requestId"])) ?? fallback.requestId,
    timestampMs: timestamp ?? fallback.timestampMs ?? null,
    precedingText,
    preedit,
    rimeCandidates: rime,
    finalCandidates: final,
    diagnostics: diagnosticsFrom(diagnosticsRaw, rime, final),
    llmPerformance: normalizeLlmPerformance(performanceRaw) ?? fallback.llmPerformance ?? null,
    rimeMs: rimeDuration ?? fallback.rimeMs ?? (normalizeLlmPerformance(performanceRaw)?.rimeMs ?? null),
    model: model ?? fallback.model ?? null,
    contextUsed: asBoolean(firstValue(record, ["context_used", "contextUsed"])) ?? fallback.contextUsed ?? null,
    serviceState: normalizeState(firstValue(record, ["service_state", "serviceState", "state"]) ?? fallback.serviceState),
  };
}

function normalizeConfigSnapshot(value: unknown): ConfigSnapshot {
  const record = asRecord(value);
  const configRecord = asRecord(firstValue(record, ["config"])) ?? record;
  const config = { ...defaultConfig } as Config;
  for (const key of Object.keys(defaultConfig) as (keyof Config)[]) {
    const valueForKey = configRecord?.[key];
    if (typeof defaultConfig[key] === "boolean") {
      if (typeof valueForKey === "boolean") config[key] = valueForKey as never;
    } else if (key === "rime_schema" || key === "llm_backend") {
      if (typeof valueForKey === "string" && valueForKey) config[key] = valueForKey as never;
    } else {
      const numberValue = asNumber(valueForKey);
      if (numberValue != null) config[key] = numberValue as never;
    }
  }
  return { revision: asNumber(firstValue(record, ["revision", "config_revision", "configRevision"])) ?? 0, config };
}

function normalizeModel(value: unknown): ModelInfo {
  const record = asRecord(value);
  const memory: Record<string, number> = {};
  const collectMemory = (candidate: unknown, prefix = "") => {
    const nested = asRecord(candidate);
    if (!nested) return;
    for (const [key, nestedValue] of Object.entries(nested)) {
      if (!prefix && key === "breakdown" && asRecord(nestedValue)) {
        collectMemory(nestedValue);
        continue;
      }
      const label = prefix ? prefix + "." + key : key;
      const numeric = asNumber(nestedValue);
      if (numeric != null && Number.isFinite(numeric) && numeric >= 0) memory[label] = numeric;
      else if (asRecord(nestedValue)) collectMemory(nestedValue, label);
    }
  };
  const memoryValue = firstValue(record, [
    "memory", "memory_usage", "memoryUsage", "vram", "vram_usage", "vramUsage",
    "gpu_memory", "gpuMemory", "gpu_mem", "gpuMem", "initialization_memory", "initializationMemory",
    "init_memory", "initMemory",
  ]);
  const memoryRecord = asRecord(memoryValue);
  if (memoryRecord?.breakdown && asRecord(memoryRecord.breakdown)) {
    collectMemory(memoryRecord.breakdown);
  } else {
    collectMemory(memoryValue);
  }
  if (record) {
    for (const [key, nestedValue] of Object.entries(record)) {
      if (!/(memory|vram|gpu_mem|offload|buffer)/i.test(key)) continue;
      const numeric = asNumber(nestedValue);
      if (numeric != null && Number.isFinite(numeric) && numeric >= 0) memory[key] = numeric;
    }
  }
  return {
    path: (firstValue(record, ["path", "model_path", "modelPath"]) as string | null | undefined) ?? null,
    size_bytes: asNumber(firstValue(record, ["size_bytes", "sizeBytes"])),
    sha256: (firstValue(record, ["sha256", "sha_256"]) as string | null | undefined) ?? null,
    loaded: Boolean(firstValue(record, ["loaded", "is_loaded", "isLoaded"])),
    memory,
  };
}

function normalizePreset(value: unknown): ModelPreset | null {
  const record = asRecord(value);
  if (!record) return null;
  const name = asString(firstValue(record, ["name", "label", "id"]));
  const pathValue = firstValue(record, ["path", "model_path", "modelPath"]);
  if (!name && typeof pathValue !== "string") return null;
  const path = typeof pathValue === "string" ? pathValue : null;
  return {
    id: asString(firstValue(record, ["id", "key", "name"]), name || path || ""),
    name: name || path || "未命名预设",
    path,
    sizeBytes: asNumber(firstValue(record, ["size_bytes", "sizeBytes"])),
    sha256: (firstValue(record, ["sha256", "sha_256"]) as string | null | undefined) ?? null,
    loaded: Boolean(firstValue(record, ["loaded", "is_loaded", "isLoaded"])),
  };
}

function normalizePresets(value: unknown): ModelPreset[] {
  const record = asRecord(value);
  const values = Array.isArray(value) ? value : firstValue(record, ["presets", "items", "models"]);
  if (!Array.isArray(values)) return [];
  return values.map(normalizePreset).filter((preset): preset is ModelPreset => preset !== null);
}

function normalizeHistoryPage(value: unknown, page: number): HistoryPage {
  const record = asRecord(value);
  const values = Array.isArray(value) ? value : firstValue(record, ["items", "entries", "history"]);
  const items = Array.isArray(values) ? values.map((item) => normalizeInputData(item)) : [];
  const total = asNumber(firstValue(record, ["total", "total_count", "totalCount"])) ?? items.length;
  const returnedPage = asNumber(firstValue(record, ["page", "page_number", "pageNumber"])) ?? page;
  const pageSize = asNumber(firstValue(record, ["page_size", "pageSize"])) ?? HISTORY_PAGE_SIZE;
  return { items, total, page: returnedPage, pageSize };
}

function normalizeDictionaryEntry(value: unknown): DictionaryEntry | null {
  const record = asRecord(value);
  if (!record) return null;
  const pinyin = asString(firstValue(record, ["pinyin", "code"]));
  const text = asString(firstValue(record, ["text", "word"]));
  const weight = asNumber(firstValue(record, ["weight", "score"])) ?? 0;
  return pinyin && text ? { pinyin, text, weight } : null;
}

function normalizeDictionaryPage(value: unknown, page: number): DictionaryPage {
  const record = asRecord(value);
  const values = Array.isArray(value) ? value : firstValue(record, ["items", "entries", "dictionary"]);
  const items = Array.isArray(values)
    ? values.map(normalizeDictionaryEntry).filter((entry): entry is DictionaryEntry => entry !== null)
    : [];
  const total = asNumber(firstValue(record, ["total", "total_count", "totalCount"])) ?? items.length;
  const returnedPage = asNumber(firstValue(record, ["page", "page_number", "pageNumber"])) ?? page;
  const pageSize = asNumber(firstValue(record, ["page_size", "pageSize"])) ?? DICTIONARY_PAGE_SIZE;
  return { items, total, page: returnedPage, pageSize };
}

async function invokeVariants<T>(command: string, variants: Array<Record<string, unknown> | undefined>): Promise<T> {
  let firstError: unknown;
  let lastError: unknown = new Error("命令调用失败");
  for (const args of variants) {
    try {
      return await invoke<T>(command, args);
    } catch (error) {
      firstError ??= error;
      lastError = error;
    }
  }
  // Preserve a meaningful domain error from the canonical argument shape when a
  // compatibility retry fails because that shape is unsupported.
  throw firstError ?? lastError;
}

async function fetchHistoryPage(page: number): Promise<HistoryPage> {
  try {
    const value = await invokeVariants<unknown>("get_input_history_page", [
      { page, pageSize: HISTORY_PAGE_SIZE },
      { page, page_size: HISTORY_PAGE_SIZE },
    ]);
    return sortHistory(normalizeHistoryPage(value, page));
  } catch (error) {
    // Do not fall back to the legacy unbounded history response: a long-running
    // service can exceed the local IPC frame limit before the UI can paginate it.
    throw error;
  }
}

async function fetchDictionaryPage(page: number): Promise<DictionaryPage> {
  const value = await invokeVariants<unknown>("get_dictionary_page", [
    { page, pageSize: DICTIONARY_PAGE_SIZE },
    { page, page_size: DICTIONARY_PAGE_SIZE },
  ]);
  return normalizeDictionaryPage(value, page);
}

async function fetchAllDictionaryEntries(): Promise<DictionaryEntry[]> {
  const first = await fetchDictionaryPage(1);
  const totalPages = Math.max(1, Math.ceil(first.total / DICTIONARY_PAGE_SIZE));
  const entries = [...first.items];
  for (let page = 2; page <= totalPages; page += 1) {
    const next = await fetchDictionaryPage(page);
    entries.push(...next.items);
  }
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
  if (size) size.textContent = model?.size_bytes == null ? "—" : formatBytes(model.size_bytes);
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
  return '<td>' + escapeHtml(entry.precedingText || "（空）") + '</td><td class="mono">' + escapeHtml(entry.preedit || "—") + "</td><td>" + escapeHtml(candidateText(rime)) + "</td><td>" + escapeHtml(candidateText(llm)) + "</td><td class=\"mono numeric\">" + escapeHtml(formatMilliseconds(entry.llmPerformance?.totalMs)) + "</td><td>" + escapeHtml(entry.model || "—") + "</td>";
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

function llmPerformanceSummary(performance: LlmPerformance | null, rimeMs: number | null): string {
  if (!performance && rimeMs == null) return '<p class="muted">本次未调用 LLM。</p>';
  const row = (label: string, value: string) => '<dt>' + label + '</dt><dd class="mono">' + escapeHtml(value) + '</dd>';
  const effectiveRimeMs = rimeMs ?? performance?.rimeMs ?? null;
  return '<dl class="status-list history-performance">' +
    row("Rime 耗时", formatMilliseconds(effectiveRimeMs)) +
    row("LLM 总耗时", formatMilliseconds(performance?.totalMs)) +
    row("Token 化", formatMilliseconds(performance?.tokenizeMs)) +
    row("推理解码", formatMilliseconds(performance?.decodeMs)) +
    row("Logits 计算", formatMilliseconds(performance?.logitsMs)) +
    row("送入候选", performance ? String(performance.candidateCount) + " 个" : "—") +
    row("返回得分", performance ? String(performance.scoredCount) + " 个" : "—") +
    row("目标 Token", performance ? String(performance.targetTokenCount) : "—") +
    row("解码批次", performance ? String(performance.batchCount) : "—") +
    row("边界不匹配", performance ? String(performance.mismatchCount) + " 个" : "—") +
    row("上下文 Token", performance ? String(performance.contextTokenCount) : "—") +
    row("Decode 输入行", performance ? String(performance.decodeInputTokenCount) : "—") +
    row("Logits 输出行", performance ? String(performance.logitsOutputCount) : "—") +
    '</dl>';
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
  target.innerHTML = '<div class="result-summary"><span>服务状态</span><strong>' + escapeHtml(status) + '</strong><span>上文已使用</span><strong>' + context + '</strong><span>拼音</span><strong class="mono">' + escapeHtml(data.preedit || "—") + "</strong></div>" + diagnosticTable(data);
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
  content.innerHTML = llmPerformanceSummary(entry.llmPerformance, entry.rimeMs) + diagnosticTable(entry);
  detail.scrollIntoView?.({ behavior: "smooth", block: "start" });
}

function closeHistoryDetail() {
  query<HTMLElement>("[data-history-detail]")?.classList.add("is-hidden");
  queueMicrotask(flushDeferredRenders);
}

async function fetchModelPresets(): Promise<ModelPreset[]> {
  const value = await invokeVariants<unknown>("list_model_presets", [undefined, {}]);
  return normalizePresets(value);
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
    const key = escapeHtml(preset.id || preset.name);
    const loaded = preset.loaded ? " is-loaded" : "";
    const fullPath = preset.path || "—";
    return '<article class="preset-item' + loaded + '"><div class="preset-info"><strong>' + escapeHtml(preset.name) + '</strong><span class="muted mono" title="' + escapeHtml(fullPath) + '">' + escapeHtml(fullPath === "—" ? fullPath : truncatePath(fullPath, 56)) + '</span></div><div class="preset-actions"><button class="button button-primary" type="button" data-preset-action="select" data-preset-key="' + key + '">切换</button><button class="button button-danger" type="button" data-preset-action="delete" data-preset-key="' + key + '">删除</button></div></article>';
  }).join("");
  renderedPresetsKey = key;
}

async function performRefresh(reason: RefreshReason, mutationAtStart: number) {
  const historyAtStart = historyRefreshEpoch;
  const results = await Promise.allSettled([
    invoke<unknown>("get_config"),
    invoke<unknown>("get_status"),
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
  if (configResult.status === "fulfilled") configSnapshot = normalizeConfigSnapshot(configResult.value);
  else errors.push(configResult.reason);

  let statusConfig: ConfigSnapshot | null = null;
  if (statusResult.status === "fulfilled") {
    const rawStatus = asRecord(statusResult.value);
    const rawStatusConfig = firstValue(rawStatus, ["config"]);
    if (rawStatusConfig != null) statusConfig = normalizeConfigSnapshot(rawStatusConfig);
    const fallbackStatusConfig = statusConfig ?? configSnapshot ?? currentStatus?.config ?? {
      revision: 0,
      config: { ...currentConfig },
    };
    const status: ServiceStatus = {
      state: normalizeState(firstValue(rawStatus, ["state", "service_state", "serviceState"])),
      config: fallbackStatusConfig,
      model: normalizeModel(firstValue(rawStatus, ["model"]) ?? {}),
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
      const value = await invoke<unknown>("wait_for_input_history", { revision: historyRevision });
      const nextRevision = asNumber(value);
      if (nextRevision == null) throw new Error("历史更新通知无效");
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
    || previousConfig.llm_backend !== nextConfig.llm_backend
  ));
  try {
    const value = await invoke<unknown>("set_config", { config: nextConfig });
    const snapshot = normalizeConfigSnapshot(value);
    applyConfig(snapshot, { force: true });
    if (currentStatus) currentStatus.config = snapshot;
    renderStatus(currentStatus);
    applyThemeMode(query<HTMLSelectElement>("[data-theme-mode]")?.value, true);
    if (reloadModel && activeModelPath) {
      try {
        const modelValue = await invokeVariants<unknown>("load_model", [{ path: activeModelPath }, { modelPath: activeModelPath }]);
        const model = normalizeModel(modelValue);
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
    const value = await invokeVariants<unknown>("load_model", [{ path }, { modelPath: path }]);
    const model = normalizeModel(value);
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
    const value = await invoke<unknown>("unload_model");
    const model = normalizeModel(value);
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
    await invokeVariants<unknown>("save_model_preset", [{ name, path }, { preset: { name, path } }]);
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
  const preset = currentPresets.find((item) => item.id === key || item.name === key);
  if (!preset) return;
  const action = button.dataset.presetAction;
  markMutation();
  try {
    if (action === "select") {
      // The Tauri command and protocol identify a preset by its persisted name.
      // `id` is only a UI-derived alias; sending it first makes Tauri reject the
      // request before the canonical `{ name }` payload is attempted, and the
      // compatibility helper would then surface that misleading first error.
      const value = await invoke<unknown>("select_model_preset", { name: preset.name });
      const model = normalizeModel(value);
      if (model.path !== null || model.loaded) {
        renderModel(model);
        if (currentStatus) currentStatus.model = model;
      }
      await loadModelPresets({ force: true });
      setNotice("已切换到预设：" + preset.name, "success");
      recordOperation("模型预设已切换");
    } else if (action === "delete") {
      if (!window.confirm("确定删除模型预设“" + preset.name + "”吗？")) return;
      await invoke("delete_model_preset", { name: preset.name });
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
    await invoke("import_dictionary", { entries });
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
    await invoke("clear_dictionary");
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
    const value = await invokeVariants<unknown>("test_input", [
      { precedingText, preedit },
      { preceding_text: precedingText, preedit },
    ]);
    const response = normalizeInputData(value, { precedingText, preedit });
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
  if (result) result.innerHTML = '<p class="muted">输入上文和拼音后查看服务结果。</p>';
  const request = query<HTMLElement>("[data-test-request]");
  if (request) request.textContent = "尚未请求";
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
    await invoke("clear_input_history");
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
    for (const item of all<HTMLButtonElement>("[data-tab]")) item.classList.toggle("is-active", item === tab);
    for (const panel of all<HTMLElement>("[data-panel]")) panel.classList.toggle("is-hidden", panel.dataset.panel !== name);
    flushDeferredRenders();
    void requestRefresh("tab");
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

function escapeHtml(value: unknown): string {
  return String(value ?? "").replace(/[&<>'"]/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;" })[character] ?? character);
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return bytes + " B";
  if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + " KiB";
  return (bytes / (1024 * 1024)).toFixed(1) + " MiB";
}

function formatDecimal(value: number): string {
  return Number.isFinite(value) ? value.toFixed(2) : "—";
}

function formatMilliseconds(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return "—";
  return (Number.isInteger(value) ? String(value) : value.toFixed(2)) + " ms";
}

function formatLogprobs(values: number[], aggregate: number | null): string {
  if (!values.length) return "—";
  const rawSum = values.reduce((sum, value) => sum + value, 0);
  const targetCents = Math.round((aggregate == null ? rawSum : aggregate) * 100);
  const cents = values.map((value) => Math.round(value * 100));
  const prefixSum = cents.slice(0, -1).reduce((sum, value) => sum + value, 0);
  cents[cents.length - 1] = targetCents - prefixSum;
  return cents.map((value) => formatDecimal(value / 100)).join(", ");
}

function formatTimestamp(value: number | null): string {
  if (value == null || !Number.isFinite(value)) return "时间未知";
  const milliseconds = value < 1_000_000_000_000 ? value * 1000 : value;
  const date = new Date(milliseconds);
  return Number.isNaN(date.getTime()) ? "时间未知" : date.toLocaleString("zh-CN");
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

window.addEventListener("beforeunload", () => { historyWatchStopped = true; });
document.addEventListener("visibilitychange", () => { void requestRefresh("visibility"); });

window.setInterval(() => {
  void requestRefresh("poll");
}, REFRESH_INTERVAL_MS);

void requestRefresh("initial");
void watchInputHistory();
