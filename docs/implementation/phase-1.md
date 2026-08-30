# Phase 1 实现记录

日期：2026-08-30

## 目标

落地 Rust 核心服务的可运行边界：配置唯一所有者、输入请求处理、Rime 候选引擎、模型生命周期、Rime-only 降级、代际取消、词库持久化和本地 IPC 帧协议。

## 已落地

- `lime-protocol` 扩展状态、模型和词库管理契约；保持 `Candidate` 只包含展示文本与提交文本。
- `lime-core::RimeEngine` 提供统一候选引擎接口、候选完整性过滤、学习、导入/导出和清空。默认内置最小离线词表，后续可由动态 librime 适配器替换，不改变服务接口。
- `lime-core::LlamaRuntime` 实现本地 GGUF 文件校验、大小与 SHA-256 指纹、加载/卸载状态和稳定评分接口。没有模型时服务保持 `rime_only`；评分适配器可替换为 llama.cpp 完整 vocabulary logits。
- `rerank_candidates` 实现 `llm_rerank_count` / `llm_effective_count` 语义：只置顶有效重排结果，其余候选保留 Rime 原始顺序；无模型时严格保持 Rime 顺序。
- `CoreService` 实现配置 revision 校验、请求 generation、输入响应、模型切换、词库学习与原子 JSON 持久化。
- `lime-service` 二进制提供 Unix Domain Socket 服务；Windows 构建使用同一帧协议和本地 Named Pipe 入口，服务启动时通过命名互斥体保证单实例。发布启动器应为管道应用 SID ACL。
- IPC 帧具备长度上限和 JSON 解析错误处理；握手版本不匹配直接拒绝。

## 数据与隐私

词库仅写入用户数据目录的 `dictionary.json`，通过临时文件后原子替换。服务接口不记录或回传原始前文、preedit、候选分数和 prompt；模型路径、大小、SHA-256 与状态可通过管理状态查询。

## 验证

已运行并通过：

```powershell
cargo fmt --all
cargo check --workspace
cargo test --workspace
```

测试覆盖默认配置与原子更新、候选召回/学习、无模型 Rime 顺序保持、代际失效、服务输入响应和协议既有序列化契约。

本地启动（Unix 调试通道）：

```powershell
cargo run -p lime-core --bin lime-service
```

默认 socket 为 `/tmp/lime-core.sock`，可通过 `LIME_SOCKET` 和 `LIME_DATA_DIR` 覆盖；Windows 使用 `LIME_PIPE` 覆盖 Named Pipe 名称。

## 资源边界

仓库当前没有提交 librime DLL、雾凇二进制资源或 GGUF 模型（见 `resources/README.md`）。因此 Phase 1 默认路径在无本地模型时可运行 Rime-only；将固定版本运行时放入发布资源后，可在不改 IPC/TSF 接口的情况下接入原生动态后端。
