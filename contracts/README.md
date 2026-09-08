# 机器可读契约

此目录集中维护当前 IPC 和配置契约。Rust `lime-protocol`、Tauri 命令和 TSF JSON 解析必须与这里的字段保持一致。

- `config.schema.json` 与 `config.defaults.json`：管理配置。
- `persistence.schema.json`、`config.persisted.example.json` 与 `model-presets.persisted.example.json`：服务当前持久化文件格式。
- `ipc.schema.json`：输入和管理 IPC。
- `errors.json` 与 `errors.catalog.json`：错误码。
- `ipc.*.example.json`：输入协议示例；`ipc.management.*.example.json` 覆盖带 payload 的管理请求和响应。

不再维护旧版本字段或旧 payload 形状。字段变化时，同步更新 Rust 类型、前端调用和示例，并在 CI 中校验所有示例。
