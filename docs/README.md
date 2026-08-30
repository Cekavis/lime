# Lime 文档总览

本目录是 Lime 的设计与实现文档统一入口。文档按“设计约束—实现计划—决策记录”组织。

## 设计

1. [架构设计](design/architecture.md)：进程、模块、平台边界和生命周期。
2. [IPC 与数据契约](design/ipc.md)：本机通信、最小消息和错误行为。
3. [候选与 LLM 排序](design/ranking.md)：Rime 召回、llama.cpp 评分和候选合并。
4. [配置与数据目录](design/configuration.md)：设置、模型、Rime 用户数据和日志。
5. [UI 与 Design Tokens](design/ui.md)：Tauri 管理界面、原生候选窗口和 token 规则。

## 实现

- [实现计划](implementation/roadmap.md)
- [验证与发布](implementation/quality-and-release.md)

## 决策

- [决策记录](decisions/decision-log.md)

## 文档规则

- 行为、字段、默认值或平台范围发生变化时，必须同步更新相关文档。
- 每次代码变更在 PR/commit 描述中引用受影响的文档路径。
- 未决问题集中放在决策记录的“待确认”小节，不散落在实现文档中。
