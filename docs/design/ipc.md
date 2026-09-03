# IPC 与数据契约

## 传输

- Windows：当前用户 SID 专属 Named Pipe。
- macOS：用户私有运行时目录中的 Unix Domain Socket，权限 `0600`。
- 不监听 TCP/UDP，不接受局域网连接。
- 所有服务在同一发布版本中，采用简单握手；版本不匹配直接拒绝连接，不实现旧客户端兼容层。

## 请求

生产输入请求只保留必要字段：

```text
InputRequest {
  request_id: u64
  preedit: string
  preceding_text: string
  context_available: bool
  config_revision: u64
}
```

- `preceding_text` 已由平台层按字符窗口裁剪。
- 不传光标后文本、完整文档、窗口标题、应用名称、用户身份或控件类型。
- `context_available=false` 表示读取失败；服务仍按空上下文运行。
- `config_revision` 用于丢弃旧设置请求，不用于跨版本兼容。

## 响应

```text
InputResponse {
  request_id: u64
  candidates: Candidate[]
  context_used: bool
  service_state: ready | rime_only | reloading | unavailable
  diagnostics: CandidateDiagnostic[]
}

Candidate {
  display_text: string
  commit_text: string
}

CandidateDiagnostic {
  rank: u32
  rime_candidate: Candidate?
  llm_candidate: Candidate?
  logprob: f64
  logprobs: f64[]
  mismatch: bool
  display_candidate: Candidate?
}
```

前端/TSF 根据 `candidates` 数组顺序生成页码和选中状态。`diagnostics` 只供用户主动打开的测试/历史详情页使用，不应在原生候选窗口展示。启用模型时，`logprob` 与 `logprobs` 来自真实 llama.cpp vocabulary logits，且 `logprobs` 的和与 `logprob` 一致（允许浮点误差）；没有模型时该行保持 Rime-only 语义。`mismatch` 使用同一 GGUF 的 llama.cpp tokenizer 检查上文/候选边界。

## 管理 API

Tauri 使用同一 IPC 通道调用配置、模型、词库和输入诊断操作。除用户主动请求的历史接口外，管理响应不回传输入原文；详细信息写结构化日志。Phase 1 使用 4 字节 little-endian 长度前缀 + UTF-8 JSON 帧，单帧上限 16 MiB；Unix 使用用户私有 socket，Windows 使用本地 Named Pipe。

管理请求包括 `get_config`、`set_config`、`get_status`、`load_model`、`unload_model`、`list_model_presets`、`save_model_preset`、`delete_model_preset`、`select_model_preset`、`learn`、`export_dictionary`、`import_dictionary`、`clear_dictionary`、`get_input_history`、`get_input_history_page` 和 `clear_input_history`。模型导入仅接受本地 GGUF 文件，失败不会替换当前模型；模型预设保存于服务数据目录的 `model-presets.json`，切换失败时保留当前模型。

模型预设命令按名称寻址：`save_model_preset` 使用 `{ name, path }`，`delete_model_preset`
和 `select_model_preset` 使用 `{ name }`。`ModelPreset` 不定义独立的 `id` 或 `key` 字段。

`get_input_history` 返回服务本次启动后收到的全部输入请求，按 `timestamp_ms` 从新到旧排序，包含上文、拼音、Rime 原始候选、LLM 排序、诊断行和最终候选顺序；历史只保存在服务内存中，用户可在管理窗口清空。新 UI 使用 `get_input_history_page { page, page_size }`，页码从 1 开始，服务将单页大小限制为 100，并返回 `items`、`total`、`page` 和 `page_size`。`request_id` 仅为旧客户端兼容字段，不作为 UI 排序或关联依据。
- Windows TSF 默认连接 `\\.\pipe\lime-core-v1`，可由 `LIME_PIPE` 覆盖；若设置 `LIME_SERVICE_PATH`，TSF 首次连接失败时按需启动本地服务并重试。TSF 在首次握手后读取 `get_status.config.revision`，所有输入请求携带该 revision。

## 过期请求

服务为每个输入会话维护 generation。新请求到来时取消或标记旧重排任务；旧 `request_id` 的响应不得覆盖当前候选。

## 故障

- IPC 断开/握手失败：TSF 进入英文透传。
- 服务可用但 GGUF 不可用：返回 Rime 原始顺序，`service_state=rime_only`。
- Rime 初始化失败：服务不可用，TSF 英文透传。
- 候选响应解析失败：保留当前候选；若无候选则英文透传。
