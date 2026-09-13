# IPC 与数据契约

## 传输

- Windows：使用 Named Pipe `\\.\pipe\lime-core-v1`。为兼容开始菜单、系统设置等
  Windows packaged/AppContainer 宿主，管道 ACL 允许普通客户端、低完整性客户端和
  `ALL APPLICATION PACKAGES`；当前版本不额外做调用方身份校验或用户隔离。
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
  candidate_extension_of: u64?
  candidate_limit: u32
}
```

- `preceding_text` 已由平台层按字符窗口裁剪。
- 不传光标后文本、完整文档、窗口标题、应用名称、用户身份或控件类型。
- `context_available=false` 表示读取失败；服务仍按空上下文运行。
- `config_revision` 用于丢弃旧设置请求，不用于跨版本兼容。
- `candidate_extension_of` 仅用于候选窗口翻页超出已加载范围时的延迟 Rime 扩展，值为原始输入请求的
  `request_id`。服务对此类请求只加载 Rime 候选，不调用 LLM、不创建新的历史记录；成功加载的候选会追加到原历史记录，
  已返回的最终排序前缀保持不变。
- `candidate_limit` 表示客户端本次需要返回的候选前缀长度；零表示显式请求完整列表。测试页按当前页请求，
  Windows TSF 首次也按当前页请求。服务可在内部读取并重排更多候选，但 `candidates` 和
  `candidate_remainders` 只返回请求的前缀；管理用 `diagnostics` 可保留完整的内部重排行。
  翻页超出已加载范围后再请求更长前缀。

## 响应

```text
InputResponse {
  request_id: u64
  candidates: Candidate[]
  candidate_remainders: string?[]
  context_used: bool
  service_state: ready | rime_only | reloading | unavailable
  diagnostics: CandidateDiagnostic[]
  end_to_end_duration_ms: u64?
  rime_duration_ms: u64?
  llm_performance: LlmPerformance?
}

`candidate_remainders` 与 `candidates` 按位置对应：选择候选后仍需保留的原始拼音放在这里；空字符串表示候选覆盖全部输入，`null` 表示引擎无法提供可靠的选择覆盖信息。Windows TSF 用它实现小狼毫式的部分候选选择：候选文字上屏，剩余拼音继续留在组合串中。

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

InputHistoryEntry {
  request_id: u64
  timestamp_ms: u64
  end_to_end_duration_ms: u64?
  preceding_text: string
  preedit: string
  rime_candidates: Candidate[]
  final_candidates: Candidate[]
  service_state: ready | rime_only | reloading | unavailable
  model_name: string?
  rime_duration_ms: u64?
  diagnostics: CandidateDiagnostic[]
  llm_performance: LlmPerformance?
}

LlmPerformance {
  total_ms: u64
  tokenize_ms: u64
  decode_ms: u64
  candidate_count: u32
  scored_count: u32
  target_token_count: u32
  batch_count: u32
  mismatch_count: u32
  context_token_count: u32
  decode_input_token_count: u32
  logits_output_count: u32
}
```

前端/TSF 根据 `candidates` 数组顺序生成页码和选中状态。`diagnostics` 只供用户主动打开的测试/历史详情页使用，不应在原生候选窗口展示。启用模型时，`logprob` 与 `logprobs` 来自真实 llama.cpp vocabulary logits，且 `logprobs` 的和与 `logprob` 一致（允许浮点误差）；没有模型时该行保持 Rime-only 语义。`mismatch` 使用同一 GGUF 的 llama.cpp tokenizer 检查上文/候选边界；它只用于诊断，边界不匹配候选按其独立 tokenization 逐 token 追加到上文，并与其他候选共用评分路径。

## 管理 API

Tauri 使用 `lime-ipc` 的同一 IPC 通道调用配置、模型、词库和输入诊断操作。除用户主动请求的历史接口外，管理响应不回传输入原文；详细信息写结构化日志。帧使用 4 字节 little-endian 长度前缀和 UTF-8 JSON，单帧上限 16 MiB；Windows 使用兼容优先的 Named Pipe ACL，Unix 使用 `LIME_SOCKET` 指定的 Unix socket，开发默认值为 `/tmp/lime-core.sock`，部署时应放在用户私有运行时目录并设为 `0600`。

管理请求包括 `get_config`、`set_config`、`get_status`、`get_weasel_theme`、`load_model`、`unload_model`、`list_model_presets`、`save_model_preset`、`rename_model_preset`、`delete_model_preset`、`select_model_preset`、`learn`、`export_dictionary`、`get_dictionary_page`、`import_dictionary`、`clear_dictionary`、`get_input_history_page`、`wait_for_input_history` 和 `clear_input_history`。`get_weasel_theme` 只返回桌面服务读取到的 `weasel.yaml` 与 `weasel.custom.yaml` 文本，供受限 TSF 宿主应用相同主题；不返回输入内容或词库。`get_dictionary_page` 与 `get_input_history_page` 每次最多返回 100 条，管理窗口刷新和预览只使用分页接口；完整词库导出由管理窗口逐页读取后在本地拼接，避免单帧超过 16 MiB。`wait_for_input_history { revision }` 在历史 revision 变化前保持连接（最多 30 秒），响应 `input_history_revision`；管理窗口用它接收即时通知，不传输输入内容。模型导入仅接受本地 GGUF 文件，失败不会替换当前模型；模型预设保存于服务数据目录的 `model-presets.json`，切换失败时保留当前模型。

`get_status.model` 返回当前模型的路径、文件大小、SHA-256、加载状态以及可选的 `scoring_path`（`attention` 或 `recurrent`）。纯 attention 模型使用 `attention`；recurrent/hybrid 模型使用 `recurrent`。未加载模型时 `scoring_path` 为空。`initialization_memory` 是可选的 llama.cpp 初始化内存明细，包含 `model_bytes`、`context_bytes`、`compute_bytes`、`total_bytes`、`backend` 以及按设备/缓冲区分类的 `breakdown`（例如 `cuda0.model`、`cuda0.kv`、`cuda0.compute`、`cuda0.output`）；这些数值来自 llama.cpp 初始化日志，运行时无法提供日志回调时保持缺省/空值，客户端不得将空值解释为零，也不得从 GGUF 文件大小推断显存占用。

模型预设命令按名称寻址：`save_model_preset` 使用 `{ name, path }`，
`rename_model_preset` 使用 `{ name, new_name }`，`delete_model_preset` 和
`select_model_preset` 使用 `{ name }`。重命名只改变预设名称并保留路径和元数据；目标名称已存在时拒绝操作。
`ModelPreset` 不定义独立的 `id` 或 `key` 字段。

服务在同一份 `model-presets.json` 中额外保存最近一次成功激活的 `active_model_path`。直接加载模型和切换预设成功后更新该路径，卸载模型时清除；服务启动后 best-effort 恢复该路径，不阻塞 Named Pipe/Unix socket 监听和 Rime-only 路径。恢复期间 `get_status.state` 为 `reloading`；模型文件缺失或运行时不可用不会阻止服务启动。

`get_config` 和 `get_status.config` 返回完整的 `Config`；其中 `llm_ignore_emoji` 默认是 `true`，兼容旧配置缺省字段。它只影响送入 LLM 的候选池，不影响模型 native 参数，也不触发模型重载。

`get_input_history_page { page, page_size }` 返回服务本次启动后收到的输入请求，按 `timestamp_ms` 从新到旧分页，包含上文、拼音、可展示的 `model_name`（当前 GGUF 文件名；无模型时缺省）、Rime 原始候选、LLM 排序、诊断行、最终候选顺序，以及可选的 `end_to_end_duration_ms`、`rime_duration_ms` 和实际调用 LLM 时的可选 `llm_performance`。`end_to_end_duration_ms` 统计服务处理该输入请求至准备记录历史前的墙钟用时；`rime_duration_ms` 只统计候选引擎获取 Rime 候选批次的墙钟用时；`llm_performance.total_ms` 保留为纯 LLM scorer wall time，`decode_ms` 统计 native decode 调用，`logits_output_count` 表示紧凑 Logprob 结果行数而非完整词表 logits 行数，其他字段记录候选/token/batch 计数及实际送入 decode 的 token 行数。`inference_count_limit` 和 `omitted_candidate_count` 记录本次输入的推理额度以及因额度未评分的候选数量。未调用 LLM 时 `llm_performance` 为空。历史只保存在服务内存中，用户可在管理窗口清空。服务将单页大小限制为 100，并返回 `items`、`total`、`page` 和 `page_size`；`request_id` 不作为 UI 排序依据，`candidate_extension_of` 仅在服务内部用于将延迟加载的候选关联回原历史记录。
- Windows TSF 默认连接 `\\.\pipe\lime-core-v1`，可由 `LIME_PIPE` 覆盖；若设置 `LIME_SERVICE_PATH`，TSF 首次连接失败时按需启动本地服务并重试。TSF 在首次握手后读取 `get_status.config.revision`，所有输入请求携带该 revision。

## 过期请求

服务为每个输入会话维护 generation。每次请求会同步完成 Rime 召回和可用的 LLM 排序；并发请求返回前检查 generation，旧 `request_id` 的结果不得覆盖当前候选。

## 故障

- IPC 断开/握手失败：TSF 进入英文透传。
- 服务可用但 GGUF 不可用：返回 Rime 原始顺序，`service_state=rime_only`。
- Rime 初始化失败：服务不可用，TSF 英文透传。
- 候选响应解析失败：保留当前候选；若服务连接失效则英文透传。服务正常返回空候选不视为故障，TSF 保留原始组合串，等待 Enter 或 Space 提交。
