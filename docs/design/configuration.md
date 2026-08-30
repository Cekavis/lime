# 配置与数据目录

## 配置所有权

Rust 核心服务是唯一配置源。Tauri 通过 IPC 读写，TSF 只获取输入相关只读快照。

## 用户设置

```text
preceding_text_char_limit   = 128
context_preview_char_limit  = 32
page_size                   = 9
llm_rerank_count            = 32
llm_effective_count         = 3
llm_context_token_limit     = 32
llm_enabled                 = true
auto_start_service          = false
```

设置更新实时生效；模型切换需要受控重载。所有设置使用范围校验，非法值拒绝写入。

Rust 服务将配置持久化到用户数据目录的 `config.json`，格式为 `{ "version": 1, "config": { ... } }`，写入采用临时文件后原子替换。读取时兼容 Phase 3 之前直接保存的配置对象；未知版本或非法配置回退到默认值，不覆盖现有文件。

## 数据分层

- 内置资源：固定版本的 librime、雾凇拼音 schema/词库和运行时依赖。
- 用户数据：Rime userdb、自定义词组、模型文件、配置和日志，全部位于当前用户应用数据目录。
- 用户数据不被升级覆盖；内置资源升级通过独立版本目录或原子替换完成。

## 模型管理

- 用户手动导入 GGUF，不自动下载、不联网。
- 记录路径、文件大小、SHA-256 和加载状态；不要求 manifest。
- 导入新模型失败时保留当前模型；没有可用模型时进入 Rime-only。
- 同时只加载一个模型，避免内存叠加。

## 用户词库

- 首期启用 Rime 原生 userdb 和用户词组学习。
- 提供清空、导入、导出；导入先校验，失败不能覆盖现有数据。
- 清空需要二次确认。

## 隐私日志

- 默认不保存原始前文、preedit、候选或完整 prompt。
- 默认记录服务状态、模型/Rime 错误、IPC 错误和失败类型。
- 完整调试日志必须显式 opt-in，并在 UI 明确提示可能包含输入内容。
- 日志轮转、大小上限和目录由 Rust 服务统一管理。
