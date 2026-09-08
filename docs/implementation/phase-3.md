# Phase 3 实现记录

日期：2026-08-30

## 目标

完成 Tauri 2 管理窗口：设置、输入测试、模型、词库、请求历史和诊断页面通过本机管理 IPC 操作 Rust 核心服务，具备错误提示、最小状态展示和设置迁移，同时不进入 Windows TSF 实时按键路径。

## 已落地

- `src-tauri/src/ipc.rs` 实现跨平台 protocol v1 客户端：Windows 使用当前用户 Named Pipe，Unix 使用本地 socket；每次请求先握手，服务不可用时可通过 `LIME_SERVICE_PATH` 按需启动并重试。
- `src-tauri/src/main.rs` 注册配置、模型加载/卸载与多预设切换、词库、输入测试和历史分页 Tauri commands。命令只转发管理契约，不直接操作用户数据文件。
- `frontend/src/main.ts` 完成四个管理页面：
  - 输入与候选：Rime 方案与八项候选/模型配置、范围由服务端最终校验和保存反馈。
  - 模型：提交本地 GGUF 路径，显示加载状态、大小和 SHA-256，加载失败保留当前模型。
  - 词库：导入 JSON 前校验条目格式，分页读取词库（每页最多 100 条），导出 JSON 下载，清空前二次确认，并展示最多 50 条预览。
  - 诊断：服务状态、模型和词库摘要、最近操作结果。
  - 输入测试：提交与 TSF 相同的 `InputRequest`，展示包含 Rime 原始候选、LLM 候选、logprob、token logprobs、边界 mismatch 和最终展示候选的诊断表。
- 请求历史：按时间倒序分页查看（每页 100 条）输入请求，主列表显示上文/拼音、前 3 条候选和端到端用时，点击记录查看完整诊断及性能快照；可主动清空，记录只存在内存中。详情按列优先排列，集中显示端到端、Rime、推理和 Logprob 用时；性能快照同时记录 decode 输入 token 行数和结果输出行数，便于区分目标 token 数、外层 batch 数与实际 native 工作量。
- 管理窗口每 3 秒合并刷新配置、服务状态、词库和模型预设；窗口可见性变化、切换页面或手动刷新会立即触发同一流程。历史通过服务端 revision 长轮询通知，更新一发生就触发刷新，即使窗口在后台也不会依赖 WebView 定时器；历史表格按行复用，保留当前焦点和鼠标交互。过时响应会被丢弃，正在编辑的设置和预设列表交互不会被轮询打断。
- `frontend/src/style.css` 集中维护颜色、字体、间距、圆角、阴影和动效 token；页面和组件只使用语义 token。
- `crates/lime-core/src/service.rs` 增加版本化 `config.json` 原子持久化；读取兼容旧的直接 `Config` 对象，非法配置/未知版本回退默认配置。配置写入失败时恢复内存中的旧 snapshot。
- `src-tauri/build.rs` 生成被 `.gitignore` 忽略的最小占位 ICO，使无品牌资源时 Tauri 工程仍可独立检查；发布前可直接替换为正式图标资源。

## IPC 与数据边界

配置、模型加载、词库变更和输入测试均通过 Rust 核心服务完成。模型页只向服务提交用户输入的本地路径；词库导入由 Web File API 读取 JSON 后通过 `import_dictionary` 发送条目，导出由 UI 生成下载文件，服务将条目交给 librime 原生 `rime_ice.userdb` 管理。请求历史是用户主动打开的内存诊断数据，不写入结构化日志或磁盘。

## 验证

已通过：

```powershell
cargo fmt --all
cargo check --workspace --offline
cargo test --workspace --offline
cargo check --manifest-path src-tauri/Cargo.toml --offline
powershell.exe -Command "& C:\Users\cekav\AppData\Local\nvm\nvm.exe use 25; Set-Location frontend; npm run build"
```

结果：Rust workspace 测试 13 项通过；Tauri crate 离线检查通过；前端 TypeScript 检查和 Vite production build 通过。尚未执行真实 Tauri 窗口、Windows 服务进程、原生文件选择器和安装包级验证，这些属于 Phase 4 验收矩阵。

## 已知边界

- 当前模型页使用路径输入，不引入额外 dialog/fs 插件；服务协议和 UI 行为已经为后续原生文件选择器保留边界。
- `auto_start_service` 配置由 Rust 服务持久化并通过 UI 管理；进程级启动策略仍由发布启动器和 `LIME_SERVICE_PATH` 负责。
- RimeEngine 已使用原生 librime 后端并读取发布包中的完整雾凇资源；未提供 librime DLL 的开发环境会保持后端不可用。原生运行时随安装包加载及 Windows 输入链路仍属于 Phase 4 验收范围。
