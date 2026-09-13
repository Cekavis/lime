# Lime 管理窗口（Tauri 2）

这是 Lime 的 Tauri 2 管理窗口。`src/` 提供 Vite + TypeScript 的 vanilla DOM 管理页面；`src-tauri/` 注册管理命令，并通过 `lime-ipc` 和 protocol v1 调用 `lime-service`。窗口不参与实时输入，也不直接持久化配置、模型或用户词库。

## 本地运行

```powershell
npm install
npm run build
cargo check --manifest-path src-tauri/Cargo.toml --offline
```

安装 Tauri CLI 后可在本目录运行 `npm run tauri dev`，或从仓库根目录运行 `npm --prefix crates/lime-management run tauri dev`。若服务未运行，管理命令会优先使用 `LIME_SERVICE_PATH`，再从已安装管理程序旁边查找 `lime-service.exe` 并按需启动；否则页面会显示“服务不可用”。
