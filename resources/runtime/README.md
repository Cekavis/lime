# 平台运行时资源

发布脚本不会把 librime 或 llama.cpp 二进制提交到仓库。固定版本清单见
`librime-1.17.0.manifest.json`：librime 为官方 `1.17.0` Windows x64 发布包，雾凇为
官方 `2026.06.30` `full_compiled.zip`。运行
`tools/release/prepare-rime-runtime.ps1` 会下载并校验这两个发布包，输出一个标准运行时
目录；`tools/release/build-windows.ps1` 在没有设置 `LIME_RIME_RUNTIME_DIR` 时自动执行该步骤。

llama.cpp 的固定清单包括 `llama-cpp-b10743-cuda-13.3.manifest.json`、可选的
`llama-cpp-b10743-cuda-12.4.manifest.json` 和 `llama-cpp-b10743.manifest.json`，默认使用官方
`b10743` Windows x64 CUDA 13.3，另有 CPU 发布包。`tools/release/prepare-llama-runtime.ps1`
默认准备 CUDA 13.3（可传入 `-CudaVersion 12.4` 选择另一套 CUDA 资产），也可传入
`-Backend cpu` 准备 CPU 运行时；脚本会校验 SHA-256，并把共享库和 backend plugin 平铺到
目标目录。完整 Windows 发布构建会把两者分别放在 `llama/cuda/` 和 `llama/cpu/`，服务默认
选择 CUDA 13.3，CUDA DLL/GPU 初始化失败时按策略回退到 CPU。`LIME_LLAMA_RUNTIME_DIR` 可指向与
所选 backend 对应的已有运行时目录或 `llama.dll`，用于离线开发/构建；该本地覆盖只做文件
完整性/后端必需 DLL 检查，不声称已验证上游版本或 SHA-256。未设置时，脚本下载并校验固定
版本的官方运行时；其中的 `llama.dll` 未应用 Lime patch，不能用于生产紧凑结果路径。运行时不会联网下载 native code。完整发布构建
如需分别指定两套本地目录，可设置 `LIME_LLAMA_CUDA_RUNTIME_DIR` 与
`LIME_LLAMA_CPU_RUNTIME_DIR`；单一的 `LIME_LLAMA_RUNTIME_DIR` 适用于同时包含两种后端的
混合目录。

生产 logits 路径要求使用 `llama.cpp-b10743-output-reorder.patch`，避免输出重排时交换完整词表，
并在 backend sampler 已生成紧凑结果时跳过 raw logits 主机拷贝。仓库只提交可复现的 patch，
不提交 DLL；官方未 patch 的 `llama.dll` 不能提供这条生产路径。准备 b10743 源码后执行
`tools/release/apply-llama-gpu-logprob-patch.ps1 -SourceDirectory <llama.cpp 源码目录>`，
再编译 `llama.dll` 并通过 `LIME_LLAMA_CUDA_RUNTIME_DIR` 或 `LIME_LLAMA_CPU_RUNTIME_DIR`
传给发布脚本。脚本会写入 `.lime-output-reorder-patched` 标记和 patch SHA-256，供构建流程识别。
源码归档地址、commit、归档 SHA-256 和 patch SHA-256 记录在
`llama-cpp-b10743-source.manifest.json`。

准备后的目录保留雾凇发布包的 `build/`、schema、词库、Lua 和 OpenCC 文件原样，并把官方
librime DLL（及其发布依赖）放在根目录。构建只排除 `trash/`、`user/`、`*.userdb`、
`installation.yaml`、`user.yaml` 等运行状态；不会把仓库中的自定义 YAML 或自行生成的词库
混入安装包。最终 Tauri 将该目录安装到只读的 `rime/`。

服务默认选择雾凇全拼方案 `rime_ice`；需要验证或部署其他随发布包提供、且当前 Windows
运行时支持的方案时，可在启动服务前设置 `LIME_RIME_SCHEMA`（例如
`double_pinyin_flypy`）。方案文件和编译数据仍由 librime 原样读取，Lime 不改写发布资源。

发布包中的 `t9` 文件会按上游归档原样保留，但 Windows 首期不把它列为可选方案：该方案由
上游注明仅适用于特定 iOS 软件，并依赖当前官方 Windows librime DLL 未提供的
`t9_processor`。

该暂存目录属于构建产物，不得提交；用户词库和 Rime userdb 仍写入当前用户的
`%LOCALAPPDATA%\\Lime`。

Tauri 发布包将该 llama.cpp 目录安装为可执行文件旁的 `llama/`，其下包含 `cuda/` 与
`cpu/` 两套 DLL。服务按 backend 选择对应目录，并在 CUDA 13.3 不可用时尝试 `cpu/`；模型预设
只保存 GGUF 路径和校验信息，真正切换时必须成功初始化该原生运行时，否则保留当前模型
并返回明确错误。
