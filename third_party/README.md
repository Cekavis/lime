# 第三方资源

`third_party/` 是仓库内所有第三方输入的唯一入口。固定版本 manifest、许可证、默认主题、Rust wrapper、WeaselUI 快照和可重放 patch 都放在这里；下载内容、源码 checkout、编译目录和 DLL 全部写入 `out/`。

- `rime/librime-1.17.0.json`：librime 与雾凇拼音 binary 资源，以及对应源码 commit；同目录的 `weasel.yaml` 是 Lime 默认主题。
- `llama/b10743/source.json`：llama.cpp 源码 commit 与 patch provenance。
- `llama/b10743/runtime-*.json`：官方 CPU/CUDA binary runtime。
- `llama/b10743/output-reorder.patch`：Lime 必须应用的 llama.cpp patch。
- `weasel-ui/`：固定源码快照和 GPLv3 许可证。
- `llama-cpp-v3/`、`llama-cpp-sys-v3/`：Rust llama.cpp wrapper。

发布脚本从这些 manifest 出发下载并校验资源，统一写入 `out/`。
