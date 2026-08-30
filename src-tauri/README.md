# Lime 管理窗口（Tauri 2）

这是 Phase 0 的最小 Tauri 2 工程骨架。`frontend/` 提供 Vite + TypeScript 管理窗口，并预置 Tailwind/shadcn 配置与集中 design tokens；`src-tauri/` 只负责窗口生命周期。配置、模型和词库操作必须通过后续阶段的 Rust 核心 IPC 接入。

## 本地运行

```powershell
cd frontend
npm install
npm run build
cd ..\src-tauri
cargo check
```

安装 Tauri CLI 后可从仓库根目录运行 `npm --prefix frontend run tauri dev -- --config src-tauri/tauri.conf.json`。当前 scaffold 不包含 Rust 核心服务，也不会读写用户输入数据。
