# Lime

Lime 是本地优先的中文拼音输入法。Windows TSF 负责实时按键与原生候选窗口，Rust 服务负责 Rime 候选、LLM 排序、配置和用户词库，Tauri 只提供管理界面。

当前目标平台是 Windows 10 22H2+ x64。发布前仍需完成 Windows 实机验收，尤其是 TSF、Rime、CUDA/CPU runtime 和安装器行为。

- [项目状态](docs/status.md)
- [架构设计](docs/design/architecture.md)
- [IPC 与数据契约](docs/design/ipc.md)
- [候选与排序](docs/design/ranking.md)
- [配置与数据目录](docs/design/configuration.md)
- [管理界面设计](docs/design/ui.md)
- [WeaselUI 集成](docs/design/weasel-ui-integration.md)
- [构建与发布](docs/reference/build-and-release.md)
- [输入法 benchmark](docs/reference/benchmark.md)
- [机器可读契约](contracts/README.md)
- [Tauri 管理窗口](crates/lime-management/README.md)
- [Windows TSF](platform/windows/tsf/README.md)
- [第三方资源](third_party/README.md)

## 本地检查

```powershell
cargo fmt --all -- --check
cargo check --workspace
cargo check -p lime-ipc -p lime-service
cargo test --workspace
cargo check --manifest-path crates/lime-management/src-tauri/Cargo.toml
powershell.exe -Command "npm --prefix crates/lime-management test"
powershell.exe -Command "npm --prefix crates/lime-management run build"
```

Tauri 检查会验证发布资源路径；先运行资源准备脚本，或在本地创建 `out/windows-x64/staging/app/` 下的占位资源目录。

## Windows 发布

```powershell
powershell -ExecutionPolicy Bypass -File tools/build-windows.ps1
```

脚本从 `third_party/` 中的固定 manifest 下载 Rime、雾凇和 llama.cpp 源码/二进制，写入 `out/windows-x64/`，自动应用 llama.cpp patch，编译 patched `llama.dll`，再组装安装包。仓库不提交 GGUF、native DLL、Rime 大型词库或构建产物。
