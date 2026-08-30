# Lime 数据契约 schema

- `config.schema.json`：用户设置字段、默认值和 Phase 0 范围。
- `ipc.schema.json`：生产输入请求/响应的最小字段集合。
- `errors.json` + `errors.catalog.json`：统一错误码目录及其 schema。
- `config.defaults.json`、`ipc.*.example.json`：CI 使用的可校验示例。

schema 是契约的机器可读镜像；字段或默认值发生变化时，必须同步更新 Rust `lime-protocol`、`lime-core` 与设计文档。
