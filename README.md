# Lime

Lime（Language model IME）是一个本地优先的中文拼音输入法项目。它在 Rime/雾凇拼音候选召回之上，利用光标前文本和本地语言模型改善候选排序。

当前阶段：**Phase 1 Rust 核心服务已完成，Windows TSF 与 Tauri 管理 UI 按 roadmap 进入后续阶段**。

## 统一入口

- [文档总览](docs/README.md)
- [架构设计](docs/design/architecture.md)
- [IPC 与数据契约](docs/design/ipc.md)
- [候选与 LLM 排序](docs/design/ranking.md)
- [配置与数据目录](docs/design/configuration.md)
- [UI 与 Design Tokens](docs/design/ui.md)
- [实现计划](docs/implementation/roadmap.md)
- [验证与发布](docs/implementation/quality-and-release.md)
- [Phase 0 实现记录](docs/implementation/phase-0.md)
- [Phase 1 实现记录](docs/implementation/phase-1.md)
- [决策记录](docs/decisions/decision-log.md)

## 现有实验资产

- `_archive/2026-08-25-ime-context-probe/`：Windows TSF 获取光标前文本的实验代码。
- `tools/pinyin-eval/`：Rime/雾凇拼音 + llama.cpp 候选评分实验，生产核心算法首期沿用其评分语义。

实验资产用于参考和离线验证，不属于生产运行时；模型、用户数据和构建产物不进入 Git。

## 首期范围

- Windows 10 22H2+ x64 可用。
- 全拼、简体中文、中英文/数字/常用标点输入。
- CPU-only；GPU 加速后置。
- macOS 只设计平台 API，不交付可用输入法。
