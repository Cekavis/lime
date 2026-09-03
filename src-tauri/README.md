# Lime 管理窗口（Tauri 2）

这是 Phase 3 的 Tauri 2 管理窗口。`frontend/` 提供 Vite + TypeScript 管理页面与集中 design tokens；`src-tauri/` 注册管理命令并通过 protocol v1 本机 IPC 调用 Rust 核心服务。窗口不参与实时输入，也不直接持久化配置、模型或用户词库。

## 本地运行

```powershell
cd frontend
npm install
npm run build
cd ..\src-tauri
cargo check --offline
```

安装 Tauri CLI 后可从仓库根目录运行 `npm --prefix frontend run tauri dev -- --config src-tauri/tauri.conf.json`。若服务未运行，管理命令会优先使用 `LIME_SERVICE_PATH`，再从已安装管理程序旁边查找 `lime-service.exe` 并按需启动；否则页面会显示“服务不可用”。
