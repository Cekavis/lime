# 验证与发布

CI 覆盖 Rust workspace（包括 `lime-ipc`、`lime-core` 和 `lime-service`）的格式、检查、Clippy 和单元测试；`contracts/` 中的 JSON schema examples；以及 `crates/lime-management` 管理应用的前端构建和 `src-tauri` 独立 crate 检查。

输入链路是同步的：服务在一次请求中完成 Rime 召回和可用的 LLM 排序后返回最终候选。并发请求在返回前检查 generation，旧结果不得覆盖较新的输入。

Windows 发布还需验证固定第三方源码 commit、llama.cpp patch marker、CPU/CUDA runtime 必需 DLL、TSF Release 构建和 NSIS 安装包 SHA-256。发布脚本将所有中间文件和产物写入 `out/`，不会使用仓库根目录的 `target/` 或 `build/` 作为发布输入输出。

具体命令见 [构建与发布](../reference/build-and-release.md)。
