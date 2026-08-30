import { invoke } from "@tauri-apps/api/core";
import "./style.css";

type ServiceState = "ready" | "rime_only" | "reloading" | "unavailable";

interface Config {
  preceding_text_char_limit: number;
  context_preview_char_limit: number;
  page_size: number;
  llm_rerank_count: number;
  llm_effective_count: number;
  llm_context_token_limit: number;
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
  preceding_text_char_limit: 128,
  context_preview_char_limit: 32,
  page_size: 9,
  llm_rerank_count: 32,
  llm_effective_count: 3,
  llm_context_token_limit: 32,
  llm_enabled: true,
  auto_start_service: false,
};

const app = document.querySelector<HTMLDivElement>("#app");
if (!app) throw new Error("Lime UI mount point is missing");

app.innerHTML = `
  <div class="shell">
    <header class="header">
      <div><p class="eyebrow">LIME</p><h1>设置</h1></div>
      <span class="badge" data-service-state="unavailable" aria-live="polite">服务不可用</span>
    </header>

    <div class="notice is-hidden" data-notice role="status" aria-live="polite"></div>

    <nav class="tabs" aria-label="设置分类">
      <button class="tab is-active" type="button" data-tab="input">输入与候选</button>
      <button class="tab" type="button" data-tab="model">模型</button>
      <button class="tab" type="button" data-tab="dictionary">词库</button>
      <button class="tab" type="button" data-tab="diagnostics">诊断</button>
    </nav>

    <main>
      <section class="panel" data-panel="input">
        <div class="panel-heading"><div><h2>输入与候选</h2><p class="muted">修改后立即交给 Rust 核心服务保存。</p></div><span class="revision" data-config-revision>revision —</span></div>
        <form data-config-form>
          <div class="form-grid">
            <label class="field"><span>前文窗口（字符）</span><input data-config="preceding_text_char_limit" type="number" min="1" max="4096" required /></label>
            <label class="field"><span>前文预览（字符）</span><input data-config="context_preview_char_limit" type="number" min="0" max="1024" required /></label>
            <label class="field"><span>候选页大小</span><input data-config="page_size" type="number" min="1" max="20" required /></label>
            <label class="field"><span>参与重排候选数</span><input data-config="llm_rerank_count" type="number" min="1" max="128" required /></label>
            <label class="field"><span>有效重排候选数</span><input data-config="llm_effective_count" type="number" min="1" max="32" required /></label>
            <label class="field"><span>模型上下文 token</span><input data-config="llm_context_token_limit" type="number" min="1" max="4096" required /></label>
          </div>
          <label class="switch"><input data-config="llm_enabled" type="checkbox" /><span>启用本地模型重排</span></label>
          <label class="switch"><input data-config="auto_start_service" type="checkbox" /><span>启动时自动连接服务</span></label>
          <div class="actions"><button class="button button-primary" type="submit">保存设置</button></div>
        </form>
      </section>

      <section class="panel is-hidden" data-panel="model">
        <div class="panel-heading"><div><h2>模型</h2><p class="muted">仅接受本机 GGUF 文件，服务读取并校验文件。</p></div><span class="status-dot" data-model-state>未加载</span></div>
        <div class="model-card"><dl class="status-list"><dt>路径</dt><dd data-model-path>—</dd><dt>大小</dt><dd data-model-size>—</dd><dt>SHA-256</dt><dd class="mono" data-model-sha>—</dd></dl></div>
        <form class="model-form" data-model-form><label class="field field-wide"><span>GGUF 文件路径</span><input data-model-path-input type="text" placeholder="C:\\Models\\lime.gguf" required /></label><div class="actions"><button class="button button-primary" type="submit">加载模型</button><button class="button" data-unload-model type="button">卸载模型</button></div></form>
      </section>

      <section class="panel is-hidden" data-panel="dictionary">
        <div class="panel-heading"><div><h2>词库</h2><p class="muted">导入前校验 JSON；失败时不会覆盖现有词库。</p></div><span class="revision" data-dictionary-count>— 条</span></div>
        <div class="actions"><button class="button" data-import-dictionary type="button">导入 JSON</button><button class="button" data-export-dictionary type="button">导出 JSON</button><button class="button button-danger" data-clear-dictionary type="button">清空用户词库</button><input class="visually-hidden" data-dictionary-file type="file" accept="application/json,.json" /></div>
        <div class="table-wrap"><table><thead><tr><th>拼音</th><th>文本</th><th>权重</th></tr></thead><tbody data-dictionary-table><tr><td colspan="3" class="muted">尚未读取词库</td></tr></tbody></table></div>
      </section>

      <section class="panel is-hidden" data-panel="diagnostics">
        <div class="panel-heading"><div><h2>诊断</h2><p class="muted">默认不记录原始前文、preedit、候选或 prompt。</p></div><button class="button" data-refresh type="button">刷新</button></div>
        <dl class="status-list diagnostics-list"><dt>协议</dt><dd>v1</dd><dt>服务状态</dt><dd data-diagnostic-state>—</dd><dt>配置 revision</dt><dd data-diagnostic-revision>—</dd><dt>模型</dt><dd data-diagnostic-model>—</dd><dt>词库条目</dt><dd data-diagnostic-dictionary>—</dd><dt>最近操作</dt><dd data-last-operation>—</dd></dl>
      </section>
    </main>
  </div>
`;

let currentConfig = { ...defaultConfig };
let currentStatus: ServiceStatus | null = null;

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

function renderStatus(status: ServiceStatus | null) {
  const badge = query<HTMLElement>("[data-service-state]");
  const state = status?.state ?? "unavailable";
  if (badge) {
    badge.textContent = `服务 ${stateLabel[state]}`;
    badge.dataset.serviceState = state;
  }
  const configRevision = query<HTMLElement>("[data-config-revision]");
  if (configRevision) configRevision.textContent = `revision ${status?.config.revision ?? "—"}`;
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
}

function applyConfig(snapshot: ConfigSnapshot) {
  currentConfig = { ...snapshot.config };
  for (const input of all<HTMLInputElement>("[data-config]")) {
    const key = input.dataset.config as keyof Config | undefined;
    if (!key) continue;
    if (input.type === "checkbox") input.checked = Boolean(currentConfig[key]);
    else input.value = String(currentConfig[key]);
  }
}

function readConfig(): Config {
  const result = { ...currentConfig };
  for (const input of all<HTMLInputElement>("[data-config]")) {
    const key = input.dataset.config as keyof Config | undefined;
    if (!key) continue;
    if (input.type === "checkbox") result[key] = input.checked as never;
    else result[key] = Number(input.value) as never;
  }
  return result;
}

function renderDictionary(entries: DictionaryEntry[]) {
  const count = query<HTMLElement>("[data-dictionary-count]");
  if (count) count.textContent = `${entries.length} 条`;
  const diagnostic = query<HTMLElement>("[data-diagnostic-dictionary]");
  if (diagnostic) diagnostic.textContent = `${entries.length} 条`;
  const table = query<HTMLTableSectionElement>("[data-dictionary-table]");
  if (!table) return;
  const rows = entries.slice(0, 50).map((entry) => `<tr><td>${escapeHtml(entry.pinyin)}</td><td>${escapeHtml(entry.text)}</td><td>${entry.weight}</td></tr>`);
  table.innerHTML = rows.length ? rows.join("") : '<tr><td colspan="3" class="muted">词库为空</td></tr>';
}

async function refresh() {
  try {
    const [config, status, entries] = await Promise.all([
      invoke<ConfigSnapshot>("get_config"),
      invoke<ServiceStatus>("get_status"),
      invoke<DictionaryEntry[]>("export_dictionary"),
    ]);
    applyConfig(config);
    currentStatus = status;
    renderStatus(status);
    renderDictionary(entries);
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
    const snapshot = await invoke<ConfigSnapshot>("set_config", { config: readConfig() });
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
    const model = await invoke<ModelInfo>("load_model", { path });
    renderModel(model);
    if (currentStatus) currentStatus.model = model;
    setNotice("模型已加载", "success");
    recordOperation("模型已加载");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("加载模型失败");
  }
});

query<HTMLButtonElement>("[data-unload-model]")?.addEventListener("click", async () => {
  try {
    const model = await invoke<ModelInfo>("unload_model");
    renderModel(model);
    if (currentStatus) currentStatus.model = model;
    setNotice("模型已卸载，当前使用 Rime-only", "success");
    recordOperation("模型已卸载");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("卸载模型失败");
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
    renderDictionary(await invoke<DictionaryEntry[]>("export_dictionary"));
    setNotice(`已导入 ${entries.length} 条词库记录`, "success");
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
    const entries = await invoke<DictionaryEntry[]>("export_dictionary");
    const blob = new Blob([JSON.stringify(entries, null, 2)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = "lime-dictionary.json";
    link.click();
    URL.revokeObjectURL(url);
    setNotice(`已导出 ${entries.length} 条词库记录`, "success");
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
    renderDictionary(await invoke<DictionaryEntry[]>("export_dictionary"));
    setNotice("用户词库已清空", "success");
    recordOperation("词库已清空");
  } catch (error) {
    setNotice(errorMessage(error), "error");
    recordOperation("清空词库失败");
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

function escapeHtml(value: string) {
  return value.replace(/[&<>'"]/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;" })[character] ?? character);
}

function formatBytes(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

refresh();
