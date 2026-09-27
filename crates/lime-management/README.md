# Lime 管理窗口（Tauri 2）

这是 Lime 的 Tauri 2 管理窗口。`src/` 使用 Vite + React + TypeScript、Tailwind CSS 4 和 shadcn/ui；`src-tauri/` 注册管理命令，并通过 `lime-ipc` 和 protocol v1 调用 `lime-service`。窗口不参与实时输入，也不直接持久化配置、模型或用户词库。

## 本地运行

```powershell
powershell.exe -Command "npm ci"
powershell.exe -Command "npm test"
powershell.exe -Command "npm run build"
cargo check --manifest-path src-tauri/Cargo.toml --offline
```

安装 Tauri CLI 后可在本目录运行 `powershell.exe -Command "npm run tauri dev"`。若服务未运行，管理命令会优先使用 `LIME_SERVICE_PATH`，再从已安装管理程序旁边查找 `lime-service.exe` 并按需启动；否则页面会显示“服务未连接”。

## 结构

- `src/app/`：导航、主题、服务状态与操作协调。
- `src/pages/`：设置（含模型管理）、测试、评测、词库、历史。
- `src/components/ui/`：来自 shadcn/ui 的共享基础组件；变体规范见该目录 README。
- `src/components/`：分页、确认对话框、状态提示、候选与评分表格、模型管理与常显占用表等业务复用组件。
- `src/hooks/` 与 `src/lib/`：串行刷新、过期响应隔离与纯函数。
- `src/api/`：Tauri commands、响应解码和 DTO。
- `src/style.css`：集中设计 tokens 与响应式布局约定。

## 界面验证

运行 `powershell.exe -Command "npm run dev"` 后打开 `/tests/preview.html`，使用仅存在于测试页面内存中的模拟服务验证交互。正式入口没有模拟数据或开发开关，Vite 生产构建不会打包 `tests/`。

生产构建后可运行 `node tests/serve-build.mjs`，在 `http://127.0.0.1:1421` 验证同一生产产物。此测试服务器沿用 Tauri CSP，并模拟其每次响应的 style nonce 替换；可验证 Radix 弹窗锁定滚动，不需要放宽 `style-src`。关闭服务器即可停止预览。

人工检查默认 1040×760、窄窗 420×480、宽窗与深色模式；覆盖单列的设置/表格/模型顺序、双列时多设备占用表完整显示、缩放保留草稿、同页模型操作、保存重载后的占用刷新、CPU/CUDA 与未加载状态、常显候选评分、词库和历史翻页、详情焦点、评测展开/停止与 JSON 导出。模拟服务包含 CUDA、CUDA0 与 CUDA_HOST 三个设备列。真实 IPC、GGUF 加载和安装器仍需 Windows 应用实机验收。
