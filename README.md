# Lime

Lime（Language model IME）是一个本地优先的中文拼音输入法项目。它在 Rime/雾凇拼音候选召回之上，利用光标前文本和本地语言模型改善候选排序。

当前阶段：**Phase 4 发布构建已完成，待 Windows 10 22H2+ x64 实机验收**。

## 统一入口

- [文档总览](docs/README.md)
- [架构设计](docs/design/architecture.md)
- [IPC 与数据契约](docs/design/ipc.md)
- [候选与 LLM 排序](docs/design/ranking.md)
- [配置与数据目录](docs/design/configuration.md)
- [UI 与 Design Tokens](docs/design/ui.md)
- [WeaselUI 候选窗口与主题迁移](docs/design/weasel-ui-integration.md)
- [实现计划](docs/implementation/roadmap.md)
- [验证与发布](docs/implementation/quality-and-release.md)
- [Phase 0 实现记录](docs/implementation/phase-0.md)
- [Phase 1 实现记录](docs/implementation/phase-1.md)
- [Phase 2 实现记录](docs/implementation/phase-2.md)
- [Phase 3 实现记录](docs/implementation/phase-3.md)
- [Phase 4 实现记录](docs/implementation/phase-4.md)
- [Phase 4 验收矩阵](docs/implementation/phase-4-acceptance.md)
- [决策记录](docs/decisions/decision-log.md)

## 现有实验资产

- `_archive/2026-08-25-ime-context-probe/`：Windows TSF 获取光标前文本的实验代码。
- `tools/pinyin-eval/`：Rime/雾凇拼音 + llama.cpp 候选评分实验，生产核心算法首期沿用其评分语义。

实验资产用于参考和离线验证，不属于生产运行时；模型、用户数据和构建产物不进入 Git。

## 外部资源获取

仓库只提交源码、配置、资源清单和获取脚本，不提交 GGUF 模型、原生 DLL 或雾凇拼音的大型原始词库。这样可以避免把几十 MB 的上游词库和数百 MB 的 CUDA 运行时写入 Git，同时保持发布构建可复现。

准备资源需要系统提供 `curl.exe` 和 7-Zip。然后在 Windows Developer PowerShell 中运行以下命令，按固定版本下载并校验 librime、雾凇拼音和 llama.cpp 运行时；下载文件和解压目录只会写入 `target/`：

```powershell
powershell -ExecutionPolicy Bypass -File tools/release/prepare-rime-runtime.ps1
powershell -ExecutionPolicy Bypass -File tools/release/prepare-llama-runtime.ps1 -Backend cuda -CudaVersion 13.3 -OutputDirectory target/llama-runtime/cuda
powershell -ExecutionPolicy Bypass -File tools/release/prepare-llama-runtime.ps1 -Backend cpu -OutputDirectory target/llama-runtime/cpu
```

完整 Windows 发布构建会在缺少这些暂存目录时自动执行同样的步骤：

```powershell
powershell -ExecutionPolicy Bypass -File tools/release/build-windows.ps1
```

版本、上游仓库、下载地址和 SHA-256 均记录在 [`resources/runtime/`](resources/runtime/) 的 manifest 文件中。`resources/rime/` 下的大型中文/英文词库、Lua 和 OpenCC 原始文件是上游发布包的可再生副本，默认不进 Git；构建脚本会从 manifest 指定的雾凇 `full_compiled.zip` 重新取得它们。需要离线构建时，可将已校验的目录分别通过 `LIME_RIME_RUNTIME_DIR`、`LIME_LLAMA_CUDA_RUNTIME_DIR` 和 `LIME_LLAMA_CPU_RUNTIME_DIR` 传给脚本。

## 首期范围

- Windows 10 22H2+ x64 可用。
- 官方雾凇发布包内的全拼和常见双拼方案；默认使用 `rime_ice`，可在设置中切换。
- 简体中文、中英文/数字/常用标点输入。
- Windows 默认使用 CUDA 加速，并随发布包提供 CPU fallback；未检测到可用 CUDA 设备时自动回退 CPU。
- macOS 只设计平台 API，不交付可用输入法。
