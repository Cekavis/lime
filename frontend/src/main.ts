import "./style.css";

type ServiceState = "ready" | "rime_only" | "reloading" | "unavailable";

const stateLabel: Record<ServiceState, string> = {
  ready: "可用",
  rime_only: "Rime-only",
  reloading: "重载中",
  unavailable: "服务不可用",
};

const app = document.querySelector<HTMLDivElement>("#app");

if (!app) {
  throw new Error("Lime UI mount point is missing");
}

app.innerHTML = `
  <div class="shell">
    <header class="header">
      <div>
        <p class="eyebrow">LIME</p>
        <h1>管理窗口</h1>
      </div>
      <span class="badge" data-service-state="ready">服务 ${stateLabel.ready}</span>
    </header>

    <nav class="tabs" aria-label="设置分类">
      <button class="tab is-active" type="button" data-tab="input">输入与候选</button>
      <button class="tab" type="button" data-tab="model">模型</button>
      <button class="tab" type="button" data-tab="dictionary">词库</button>
      <button class="tab" type="button" data-tab="diagnostics">诊断</button>
    </nav>

    <section class="panel" data-panel="input">
      <h2>输入与候选</h2>
      <label class="field"><span>前文窗口（字符）</span><input type="number" value="128" min="0" max="1024" /></label>
      <label class="field"><span>候选页大小</span><input type="number" value="9" min="1" max="20" /></label>
      <label class="switch"><input type="checkbox" checked /><span>启用本地模型重排</span></label>
    </section>

    <section class="panel is-hidden" data-panel="model">
      <h2>模型</h2>
      <p class="muted">模型由 Rust 核心服务管理。Phase 0 仅提供管理界面占位。</p>
      <button class="button" type="button" disabled>导入 GGUF（即将支持）</button>
    </section>

    <section class="panel is-hidden" data-panel="dictionary">
      <h2>词库</h2>
      <p class="muted">Rime userdb 的导入、导出和清空将在后续阶段接入。</p>
    </section>

    <section class="panel is-hidden" data-panel="diagnostics">
      <h2>诊断</h2>
      <p class="muted">默认不记录原始前文、preedit 或候选内容。</p>
      <dl class="status-list"><dt>协议</dt><dd>Phase 0 scaffold</dd><dt>平台</dt><dd>Windows 10 22H2+ x64</dd></dl>
    </section>
  </div>
`;

for (const tab of document.querySelectorAll<HTMLButtonElement>("[data-tab]")) {
  tab.addEventListener("click", () => {
    const name = tab.dataset.tab;
    if (!name) return;
    document.querySelectorAll("[data-tab]").forEach((item) => item.classList.toggle("is-active", item === tab));
    document.querySelectorAll<HTMLElement>("[data-panel]").forEach((panel) => panel.classList.toggle("is-hidden", panel.dataset.panel !== name));
  });
}
