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
  llm_enabled: boolean;
  auto_start_service: boolean;
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

interface InputData {
  requestId?: number;
  timestampMs: number | null;
  precedingText: string;
  preedit: string;
  rimeCandidates: Candidate[];
  finalCandidates: Candidate[];
  diagnostics: CandidateDiagnostic[];
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

const stateLabel: Record<ServiceState, string> = {
  ready: "可用",
  rime_only: "Rime-only",
  reloading: "重载中",
  unavailable: "服务不可用",
};

const defaultConfig: Config = {
  rime_schema: "rime_ice",
  preceding_text_char_limit: 128,
  context_preview_char_limit: 32,
  page_size: 9,
  llm_rerank_count: 32,
  llm_effective_count: 3,
  llm_context_token_limit: 32,
  llm_backend: "cuda",
  llm_enabled: true,
  auto_start_service: false,
};

const HISTORY_PAGE_SIZE = 100;
const app = document.querySelector<HTMLDivElement>("#app");
if (!app) throw new Error("Lime UI mount point is missing");

app.innerHTML = [
  '<div class="shell">',
  '  <header class="header">',
  '    <div><p class="eyebrow">LIME</p><h1>Lime</h1><p class="muted">本地优先的中文拼音输入法</p></div>',
  '    <span class="badge" data-service-state="unavailable" aria-live="polite">服务不可用</span>',
  "  </header>",
  '  <div class="notice is-hidden" data-notice role="status" aria-live="polite"></div>',
  '  <nav class="tabs" aria-label="Lime 功能">',
  '    <button class="tab is-active" type="button" data-tab="input">设置</button>',
  '    <button class="tab" type="button" data-tab="test">测试</button>',
  '    <button class="tab" type="button" data-tab="model">模型</button>',
  '    <button class="tab" type="button" data-tab="dictionary">词库</button>',
  '    <button class="tab" type="button" data-tab="history">历史</button>',
  '    <button class="tab" type="button" data-tab="diagnostics">诊断</button>',
  "  </nav>",
  "  <main>",
  '    <section class="panel" data-panel="input">',
  '      <div class="panel-heading"><div><h2>输入与候选</h2><p class="muted">修改后交给 Rust 核心服务保存。</p></div><span class="revision" data-config-revision>revision —</span></div>',
  '      <form data-config-form>',
  '        <div class="form-grid">',
  '          <label class="field"><span>Rime 方案</span><select data-config="rime_schema"><option value="rime_ice">雾凇拼音（全拼）</option><option value="double_pinyin">自然码双拼</option><option value="double_pinyin_abc">智能 ABC 双拼</option><option value="double_pinyin_mspy">微软双拼</option><option value="double_pinyin_sogou">搜狗双拼</option><option value="double_pinyin_flypy">小鹤双拼</option><option value="double_pinyin_ziguang">紫光双拼</option><option value="double_pinyin_jiajia">拼音加加双拼</option></select></label>',
  '          <label class="field"><span>前文窗口（字符）</span><input data-config="preceding_text_char_limit" type="number" min="1" max="4096" required /></label>',
  '          <label class="field"><span>前文预览（字符）</span><input data-config="context_preview_char_limit" type="number" min="0" max="1024" required /></label>',
  '          <label class="field"><span>候选页大小</span><input data-config="page_size" type="number" min="1" max="20" required /></label>',
  '          <label class="field"><span>重排候选检查范围</span><input data-config="llm_rerank_count" type="number" min="1" max="128" required /></label>',
  '          <label class="field"><span>LLM 置顶候选数</span><input data-config="llm_effective_count" type="number" min="1" max="32" required /></label>',
          '          <label class="field"><span>模型上下文 token</span><input data-config="llm_context_token_limit" type="number" min="1" max="4096" required /></label>',
          '          <label class="field"><span>LLM 后端</span><select data-config="llm_backend"><option value="cuda">CUDA（默认）</option><option value="cpu">CPU</option></select></label>',
  "        </div>",
  '        <label class="switch"><input data-config="llm_enabled" type="checkbox" /><span>启用本地模型重排</span></label>',
  '        <label class="switch"><input data-config="auto_start_service" type="checkbox" /><span>启动时自动连接服务</span></label>',
  '        <div class="actions"><button class="button button-primary" type="submit">保存设置</button></div>',
  "      </form>",
  "    </section>",
  '    <section class="panel is-hidden" data-panel="test">',
  '      <div class="panel-heading"><div><h2>输入测试</h2><p class="muted">输入上文和拼音，检查 Rime 召回与 LLM 重排是否逐行对应。</p></div><span class="revision" data-test-request>尚未请求</span></div>',
  '      <form data-test-form>',
  '        <label class="field field-wide field-stacked"><span>上文</span><textarea data-test-context rows="3" placeholder="可选：输入光标前的中文文本"></textarea></label>',
  '        <label class="field field-wide"><span>拼音</span><input data-test-preedit type="text" placeholder="例如 nihao" required /></label>',
  '        <div class="actions"><button class="button button-primary" type="submit">请求候选</button><button class="button" data-test-clear type="button">清空结果</button></div>',
  "      </form>",
  '      <div class="test-result" data-test-result><p class="muted">输入上文和拼音后查看服务结果。</p></div>',
  "    </section>",
  '    <section class="panel is-hidden" data-panel="model">',
  '      <div class="panel-heading"><div><h2>模型</h2><p class="muted">可加载 GGUF，并保存多个本机模型预设。</p></div><span class="status-dot" data-model-state>未加载</span></div>',
  '      <div class="model-card"><dl class="status-list"><dt>路径</dt><dd data-model-path>—</dd><dt>大小</dt><dd data-model-size>—</dd><dt>SHA-256</dt><dd class="mono" data-model-sha>—</dd></dl></div>',
  '      <form class="model-form" data-model-form><label class="field field-wide"><span>GGUF 文件路径</span><input data-model-path-input type="text" placeholder="C:\\Models\\lime.gguf" required /></label><div class="actions"><button class="button button-primary" type="submit">加载模型</button><button class="button" data-unload-model type="button">卸载模型</button></div></form>',
  '      <div class="preset-section"><div class="panel-heading compact-heading"><div><h3>模型预设</h3><p class="muted">点击预设即可切换模型。</p></div><span class="revision" data-preset-count>0 个</span></div>',
  '        <form class="preset-form" data-preset-form><label class="field"><span>名称</span><input data-preset-name type="text" placeholder="例如 Qwen 7B" required /></label><label class="field field-wide"><span>路径</span><input data-preset-path type="text" placeholder="C:\\Models\\lime.gguf" required /></label><button class="button" type="submit">保存预设</button></form>',
  '        <div class="preset-list" data-model-presets><p class="muted">尚未读取预设。</p></div>',
  "      </div>",
  "    </section>",
  '    <section class="panel is-hidden" data-panel="dictionary">',
  '      <div class="panel-heading"><div><h2>词库</h2><p class="muted">导入前校验 JSON；失败时不会覆盖现有词库。</p></div><span class="revision" data-dictionary-count>— 条</span></div>',
  '      <div class="actions"><button class="button" data-import-dictionary type="button">导入 JSON</button><button class="button" data-export-dictionary type="button">导出 JSON</button><button class="button button-danger" data-clear-dictionary type="button">清空用户词库</button><input class="visually-hidden" data-dictionary-file type="file" accept="application/json,.json" /></div>',
  '      <div class="table-wrap"><table><thead><tr><th>拼音</th><th>文本</th><th>权重</th></tr></thead><tbody data-dictionary-table><tr><td colspan="3" class="muted">尚未读取词库</td></tr></tbody></table></div>',
  "    </section>",
  '    <section class="panel is-hidden" data-panel="history">',
  '      <div class="panel-heading"><div><h2>历史</h2><p class="muted">按时间从新到旧显示输入记录，每页 100 条。点击记录查看完整诊断。</p></div><div class="actions-inline"><span class="revision" data-history-count>0 条</span><button class="button button-danger" data-clear-history type="button">清空历史</button></div></div>',
  '      <div class="table-wrap"><table class="history-table"><thead><tr><th>上文</th><th>拼音</th><th>Rime 候选（前 3）</th><th>LLM 排序（前 3）</th></tr></thead><tbody data-history-table><tr><td colspan="4" class="muted">暂无输入记录</td></tr></tbody></table></div>',
  '      <div class="pagination" data-history-pagination><button class="button" data-history-prev type="button">上一页</button><span data-history-page-label>第 1 页</span><button class="button" data-history-next type="button">下一页</button></div>',
  '      <section class="history-detail is-hidden" data-history-detail aria-live="polite"><div class="panel-heading compact-heading"><div><h3 data-history-detail-title>记录详情</h3><p class="muted" data-history-detail-meta>—</p></div><button class="button" data-history-detail-close type="button">返回列表</button></div><div data-history-detail-table><p class="muted">选择一条记录查看详情。</p></div></section>',
  "    </section>",
  '    <section class="panel is-hidden" data-panel="diagnostics">',
  '      <div class="panel-heading"><div><h2>诊断</h2><p class="muted">默认不记录原始前文、preedit、候选或 prompt。</p></div><button class="button" data-refresh type="button">刷新</button></div>',
  '      <dl class="status-list diagnostics-list"><dt>协议</dt><dd>v1</dd><dt>服务状态</dt><dd data-diagnostic-state>—</dd><dt>配置 revision</dt><dd data-diagnostic-revision>—</dd><dt>模型</dt><dd data-diagnostic-model>—</dd><dt>词库条目</dt><dd data-diagnostic-dictionary>—</dd><dt>最近操作</dt><dd data-last-operation>—</dd></dl>',
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

const query = <T extends Element>(selector: string) => document.querySelector<T>(selector);
const all = <T extends Element>(selector: string) => [...document.querySelectorAll<T>(selector)];

function setNotice(message: string, tone: "success" | "error" | "info" = "info") {
  const notice = query<HTMLDivElement>("[data-notice]");
  if (!notice) return;
  notice.textContent = message;
  notice.dataset.tone = tone;
  notice.classList.toggle("is-hidden", !message);
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
  return {
    path: (firstValue(record, ["path", "model_path", "modelPath"]) as string | null | undefined) ?? null,
    size_bytes: asNumber(firstValue(record, ["size_bytes", "sizeBytes"])),
    sha256: (firstValue(record, ["sha256", "sha_256"]) as string | null | undefined) ?? null,
    loaded: Boolean(firstValue(record, ["loaded", "is_loaded", "isLoaded"])),
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
  } catch {
    const value = await invoke<unknown>("get_input_history");
    const full = sortHistory(normalizeHistoryPage(value, 1));
    const start = Math.max(0, page - 1) * HISTORY_PAGE_SIZE;
    return { items: full.items.slice(start, start + HISTORY_PAGE_SIZE), total: full.total, page, pageSize: HISTORY_PAGE_SIZE };
  }
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
  const configRevision = query<HTMLElement>("[data-config-revision]");
  if (configRevision) configRevision.textContent = "revision " + (status?.config.revision ?? "—");
  const stateText = query<HTMLElement>("[data-diagnostic-state]");
  if (stateText) stateText.textContent = stateLabel[state];
  const revision = query<HTMLElement>("[data-diagnostic-revision]");
  if (revision) revision.textContent = String(status?.config.revision ?? "—");
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
  if (path) path.textContent = model?.path ?? "—";
  const size = query<HTMLElement>("[data-model-size]");
  if (size) size.textContent = model?.size_bytes == null ? "—" : formatBytes(model.size_bytes);
  const sha = query<HTMLElement>("[data-model-sha]");
  if (sha) sha.textContent = model?.sha256 ?? "—";
  const diagnostic = query<HTMLElement>("[data-diagnostic-model]");
  if (diagnostic) diagnostic.textContent = loaded ? model?.path ?? "已加载" : "未加载（Rime-only）";
  for (const preset of currentPresets) preset.loaded = Boolean(loaded && preset.path && model?.path && preset.path === model.path);
  renderModelPresets(currentPresets);
}

function applyConfig(snapshot: ConfigSnapshot) {
  currentConfig = { ...defaultConfig, ...snapshot.config };
  for (const input of all<HTMLInputElement | HTMLSelectElement>("[data-config]")) {
    const key = input.dataset.config as keyof Config | undefined;
    if (!key) continue;
    if (input instanceof HTMLInputElement && input.type === "checkbox") input.checked = Boolean(currentConfig[key]);
    else input.value = String(currentConfig[key]);
  }
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

function renderDictionary(entries: DictionaryEntry[]) {
  const count = query<HTMLElement>("[data-dictionary-count]");
  if (count) count.textContent = entries.length + " 条";
  const diagnostic = query<HTMLElement>("[data-diagnostic-dictionary]");
  if (diagnostic) diagnostic.textContent = entries.length + " 条";
  const table = query<HTMLTableSectionElement>("[data-dictionary-table]");
  if (!table) return;
  const rows = entries.slice(0, 50).map((entry) => "<tr><td>" + escapeHtml(entry.pinyin) + "</td><td>" + escapeHtml(entry.text) + "</td><td>" + entry.weight + "</td></tr>");
  table.innerHTML = rows.length ? rows.join("") : '<tr><td colspan="3" class="muted">词库为空</td></tr>';
}

function candidateText(candidates: Candidate[], limit = 3): string {
  const values = candidates.slice(0, limit).map((candidate) => candidate.displayText).filter(Boolean);
  return values.length ? values.join(" / ") : "—";
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

function renderHistory(page: HistoryPage) {
  page = sortHistory(page);
  currentHistory = page.items;
  currentHistoryPage = page.page;
  currentHistoryTotal = page.total;
  const count = query<HTMLElement>("[data-history-count]");
  if (count) count.textContent = page.total + " 条";
  const table = query<HTMLTableSectionElement>("[data-history-table]");
  if (table) {
    const rows = page.items.map((entry, index) => {
      const rime = entry.rimeCandidates.length ? entry.rimeCandidates : entry.diagnostics.map((item) => item.rimeCandidate).filter((item): item is Candidate => item !== null);
      const diagnosticLlm = entry.diagnostics.map((item) => item.llmCandidate).filter((item): item is Candidate => item !== null);
      const llm = diagnosticLlm.length ? diagnosticLlm : entry.finalCandidates;
      return '<tr class="history-row" tabindex="0" role="button" data-history-index="' + index + '"><td>' + escapeHtml(entry.precedingText || "（空）") + '</td><td class="mono">' + escapeHtml(entry.preedit || "—") + "</td><td>" + escapeHtml(candidateText(rime)) + "</td><td>" + escapeHtml(candidateText(llm)) + "</td></tr>";
    });
    table.innerHTML = rows.length ? rows.join("") : '<tr><td colspan="4" class="muted">暂无输入记录</td></tr>';
  }
  const totalPages = Math.max(1, Math.ceil(page.total / HISTORY_PAGE_SIZE));
  const label = query<HTMLElement>("[data-history-page-label]");
  if (label) label.textContent = "第 " + page.page + " / " + totalPages + " 页";
  const previous = query<HTMLButtonElement>("[data-history-prev]");
  const next = query<HTMLButtonElement>("[data-history-next]");
  if (previous) previous.disabled = page.page <= 1;
  if (next) next.disabled = page.page >= totalPages;
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
  if (meta) meta.textContent = "上文：" + (entry.precedingText || "（空）") + "　拼音：" + (entry.preedit || "—") + "　时间：" + timestamp;
  content.innerHTML = diagnosticTable(entry);
  detail.scrollIntoView?.({ behavior: "smooth", block: "start" });
}

function closeHistoryDetail() {
  query<HTMLElement>("[data-history-detail]")?.classList.add("is-hidden");
}

async function loadModelPresets() {
  try {
    const value = await invokeVariants<unknown>("list_model_presets", [undefined, {}]);
    currentPresets = normalizePresets(value);
  } catch {
    currentPresets = [];
  }
  renderModelPresets(currentPresets);
}

function renderModelPresets(presets: ModelPreset[]) {
  const count = query<HTMLElement>("[data-preset-count]");
  if (count) count.textContent = presets.length + " 个";
  const target = query<HTMLElement>("[data-model-presets]");
  if (!target) return;
  if (!presets.length) {
    target.innerHTML = '<p class="muted">尚未保存模型预设。</p>';
    return;
  }
  target.innerHTML = presets.map((preset) => {
    const key = escapeHtml(preset.id || preset.name);
    const loaded = preset.loaded ? " is-loaded" : "";
    return '<article class="preset-item' + loaded + '"><div class="preset-info"><strong>' + escapeHtml(preset.name) + '</strong><span class="muted mono">' + escapeHtml(preset.path || "—") + '</span></div><div class="preset-actions"><button class="button button-primary" type="button" data-preset-action="select" data-preset-key="' + key + '">切换</button><button class="button button-danger" type="button" data-preset-action="delete" data-preset-key="' + key + '">删除</button></div></article>';
  }).join("");
}

async function refresh() {
  try {
    const [configValue, statusValue, entriesValue, historyPage] = await Promise.all([
      invoke<unknown>("get_config"),
      invoke<unknown>("get_status"),
      invoke<unknown>("export_dictionary"),
      fetchHistoryPage(currentHistoryPage),
    ]);
    const config = normalizeConfigSnapshot(configValue);
    const rawStatus = asRecord(statusValue);
    const status: ServiceStatus = {
      state: normalizeState(firstValue(rawStatus, ["state", "service_state", "serviceState"])),
      config: normalizeConfigSnapshot(firstValue(rawStatus, ["config"]) ?? configValue),
      model: normalizeModel(firstValue(rawStatus, ["model"]) ?? {}),
    };
    applyConfig(config);
    currentStatus = status;
    renderStatus(status);
    renderDictionary(Array.isArray(entriesValue) ? entriesValue as DictionaryEntry[] : []);
    renderHistory(historyPage);
    await loadModelPresets();
    setNotice("");
    recordOperation("已刷新");
  } catch (error) {
    currentStatus = null;
    renderStatus(null);
    setNotice(errorMessage(error), "error");
    recordOperation("刷新失败");
  }
}

query<HTMLFormElement>("[data-config-form]")?.addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    const value = await invoke<unknown>("set_config", { config: readConfig() });
    const snapshot = normalizeConfigSnapshot(value);
    applyConfig(snapshot);
    if (currentStatus) currentStatus.config = snapshot;
    renderStatus(currentStatus);
    setNotice("设置已保存", "success");
    recordOperation("设置已保存");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("保存设置失败");
  }
});

query<HTMLFormElement>("[data-model-form]")?.addEventListener("submit", async (event) => {
  event.preventDefault();
  const input = query<HTMLInputElement>("[data-model-path-input]");
  const path = input?.value.trim() ?? "";
  if (!path) return setNotice("请输入 GGUF 文件路径", "error");
  try {
    const value = await invokeVariants<unknown>("load_model", [{ path }, { modelPath: path }]);
    const model = normalizeModel(value);
    renderModel(model);
    if (currentStatus) currentStatus.model = model;
    await loadModelPresets();
    setNotice("模型已加载", "success");
    recordOperation("模型已加载");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("加载模型失败");
  }
});

query<HTMLButtonElement>("[data-unload-model]")?.addEventListener("click", async () => {
  try {
    const value = await invoke<unknown>("unload_model");
    const model = normalizeModel(value);
    renderModel(model);
    if (currentStatus) currentStatus.model = model;
    await loadModelPresets();
    setNotice("模型已卸载，当前使用 Rime-only", "success");
    recordOperation("模型已卸载");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("卸载模型失败");
  }
});

query<HTMLFormElement>("[data-preset-form]")?.addEventListener("submit", async (event) => {
  event.preventDefault();
  const nameInput = query<HTMLInputElement>("[data-preset-name]");
  const pathInput = query<HTMLInputElement>("[data-preset-path]");
  const name = nameInput?.value.trim() ?? "";
  const path = pathInput?.value.trim() ?? "";
  if (!name || !path) return setNotice("请输入预设名称和 GGUF 路径", "error");
  try {
    await invokeVariants<unknown>("save_model_preset", [{ name, path }, { preset: { name, path } }]);
    await loadModelPresets();
    setNotice("模型预设已保存", "success");
    recordOperation("模型预设已保存");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("保存模型预设失败");
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
      await loadModelPresets();
      setNotice("已切换到预设：" + preset.name, "success");
      recordOperation("模型预设已切换");
    } else if (action === "delete") {
      if (!window.confirm("确定删除模型预设“" + preset.name + "”吗？")) return;
      await invoke("delete_model_preset", { name: preset.name });
      await loadModelPresets();
      setNotice("模型预设已删除", "success");
      recordOperation("模型预设已删除");
    }
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("模型预设操作失败");
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
    await invoke("import_dictionary", { entries });
    const updated = await invoke<unknown>("export_dictionary");
    renderDictionary(Array.isArray(updated) ? updated as DictionaryEntry[] : []);
    setNotice("已导入 " + entries.length + " 条词库记录", "success");
    recordOperation("词库已导入");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("导入词库失败");
  } finally {
    (event.target as HTMLInputElement).value = "";
  }
});

query<HTMLButtonElement>("[data-export-dictionary]")?.addEventListener("click", async () => {
  try {
    const value = await invoke<unknown>("export_dictionary");
    const entries = Array.isArray(value) ? value : [];
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
  try {
    await invoke("clear_dictionary");
    renderDictionary([]);
    setNotice("用户词库已清空", "success");
    recordOperation("词库已清空");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("清空词库失败");
  }
});

query<HTMLFormElement>("[data-test-form]")?.addEventListener("submit", async (event) => {
  event.preventDefault();
  const precedingText = query<HTMLTextAreaElement>("[data-test-context]")?.value ?? "";
  const preedit = query<HTMLInputElement>("[data-test-preedit]")?.value.trim() ?? "";
  if (!preedit) return setNotice("请输入拼音", "error");
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
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("输入测试失败");
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
  try {
    renderHistory(await fetchHistoryPage(currentHistoryPage - 1));
    closeHistoryDetail();
  } catch (error) {
    setNotice(errorMessage(error), "error");
  }
});
query<HTMLButtonElement>("[data-history-next]")?.addEventListener("click", async () => {
  const totalPages = Math.max(1, Math.ceil(currentHistoryTotal / HISTORY_PAGE_SIZE));
  if (currentHistoryPage >= totalPages) return;
  try {
    renderHistory(await fetchHistoryPage(currentHistoryPage + 1));
    closeHistoryDetail();
  } catch (error) {
    setNotice(errorMessage(error), "error");
  }
});

query<HTMLButtonElement>("[data-clear-history]")?.addEventListener("click", async () => {
  if (!window.confirm("确定清空输入历史吗？")) return;
  try {
    await invoke("clear_input_history");
    currentHistoryPage = 1;
    renderHistory({ items: [], total: 0, page: 1, pageSize: HISTORY_PAGE_SIZE });
    closeHistoryDetail();
    setNotice("输入历史已清空", "success");
    recordOperation("输入历史已清空");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("清空输入历史失败");
  }
});

query<HTMLButtonElement>("[data-refresh]")?.addEventListener("click", refresh);
for (const tab of all<HTMLButtonElement>("[data-tab]")) {
  tab.addEventListener("click", () => {
    const name = tab.dataset.tab;
    if (!name) return;
    for (const item of all<HTMLButtonElement>("[data-tab]")) item.classList.toggle("is-active", item === tab);
    for (const panel of all<HTMLElement>("[data-panel]")) panel.classList.toggle("is-hidden", panel.dataset.panel !== name);
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

refresh();
