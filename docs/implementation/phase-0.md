# Phase 0 实现记录

日期：2026-08-30

## 目标

按 [实现计划](roadmap.md) 建立可持续演进的工程骨架，不提前实现 Phase 1 的实时输入服务。

## 已落地

- Rust workspace：生产 crate 与评测工具分离；`tools/pinyin-eval` 和 `_archive/2026-08-25-ime-context-probe` 保持实验资产定位。
- Rust crate：`crates/lime-protocol` 提供版本化握手、输入请求/响应、配置快照和统一错误码；`crates/lime-core` 提供配置范围校验与 revisioned 原子更新。
- Tauri 2 管理窗口：仅承载设置/模型/词库/诊断的管理壳，不进入实时按键路径；前端预置 Vite + TypeScript、Tailwind/shadcn 配置和集中 design tokens。
- Windows TSF：建立 C++ 平台适配工程与 COM/TSF 生命周期占位接口。
- 资源边界：建立 `resources/rime`、`resources/models`、`resources/runtime`，不提交模型或本机二进制。
- 实验资产：在 [实验资产边界](experimental-assets.md) 中记录归档探针和评测工具不进入生产 workspace。
- 数据契约：建立配置 schema、IPC 请求/响应 schema 和统一错误码目录，并提供可校验示例。
- 质量门禁：建立 Rust 格式化、检查、Clippy、单元测试和 schema 校验的 GitHub Actions 工作流。

配置数值范围在设计文档未明确给出，Phase 0 统一采用 `schemas/config.schema.json` 与 `lime-core::Limits` 中的保守范围，并通过测试保持两者同步。

## 验证

本地验证命令以仓库根目录的工程文件为准：

```powershell
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Windows 平台工程在具备 Visual Studio C++ 工具链时运行其 README 中的配置/构建命令；Linux CI 只验证跨平台 Rust 与 schema 门禁。2026-08-30 已通过外部 PowerShell 完成 `npm install` 与 `npm --prefix frontend run build`（Vite 6.4.3，99 个依赖，无漏洞）。同日重新安装 Visual Studio Community 2026 18.9.2 后，检测到 MSVC 19.51.36256.0、Windows SDK 10.0.26100.0、MSBuild 18.9.1 以及 VS 内置 CMake；TSF x64 Release 已成功生成 `build/tsf/Release/lime-tsf.dll`。构建仅有两个非阻断的导出符号链接器警告（LNK4104）。

## 边界与后续

Phase 0 不包含 Named Pipe 服务、Rime/llama.cpp 运行时、候选窗口、设置页面业务逻辑、安装包或签名产物；这些分别属于 Phase 1–4。
