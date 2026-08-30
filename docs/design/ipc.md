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
}

Candidate {
  display_text: string
  commit_text: string
}
```

前端/TSF 根据数组顺序生成页码和选中状态。生产响应不包含分数、延迟、候选来源、token、prompt、Rime 内部词频或 debug 字段。

## 管理 API

Tauri 使用同一 IPC 通道调用配置、模型和词库管理操作。管理响应可以包含状态和错误码，但不回传输入原文；详细信息写结构化日志。Phase 1 使用 4 字节 little-endian 长度前缀 + UTF-8 JSON 帧，单帧上限 16 MiB；Unix 使用用户私有 socket，Windows 使用本地 Named Pipe。

管理请求包括 `get_config`、`set_config`、`get_status`、`load_model`、`unload_model`、`learn`、`export_dictionary`、`import_dictionary` 和 `clear_dictionary`。模型导入仅接受本地 GGUF 文件，失败不会替换当前模型。

## 过期请求

服务为每个输入会话维护 generation。新请求到来时取消或标记旧重排任务；旧 `request_id` 的响应不得覆盖当前候选。

## 故障

- IPC 断开/握手失败：TSF 进入英文透传。
- 服务可用但 GGUF 不可用：返回 Rime 原始顺序，`service_state=rime_only`。
- Rime 初始化失败：服务不可用，TSF 英文透传。
- 候选响应解析失败：保留当前候选；若无候选则英文透传。
