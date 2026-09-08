# Phase 1 实现记录

日期：2026-08-30

## 目标

落地 Rust 核心服务的可运行边界：配置唯一所有者、输入请求处理、原生 librime/雾凇候选引擎、模型生命周期、Rime-only 降级、代际取消、词库管理和本地 IPC 帧协议。

## 已落地

- `lime-protocol` 扩展状态、模型和词库管理契约；保持 `Candidate` 只包含展示文本与提交文本。
- `lime-core::RimeEngine` 提供统一候选引擎接口、官方方案选择、学习、导入/导出和清空。候选集合和顺序由原生 librime 决定，Lime 不自行解析或猜测性过滤基础词库。默认不预置用户词库条目；发布包随附固定版本的雾凇文本资源，由原生 librime 加载，用户词库仍单独管理。
- `lime-core::LlamaRuntime` 通过 vendored llama.cpp FFI 从本地运行时目录加载 GGUF，使用模型真实 tokenizer、批量 decode 和 backend sampler 计算目标 token 的链式 logprob，并记录大小与 SHA-256 指纹。Windows 默认优先 CUDA，CUDA runtime/设备不可用时按显式策略回退 CPU；选择 CPU 时直接使用 CPU runtime。没有模型时服务保持 `rime_only`；模型/运行时初始化或推理失败会返回明确错误且不替换当前模型。
- `rerank_candidates` 实现 `llm_rerank_count` / `llm_effective_count` 语义：只置顶有效重排结果，其余候选保留 Rime 原始顺序；无模型时严格保持 Rime 顺序。
- `CoreService` 实现配置 revision 校验、请求 generation、输入响应、模型切换和原生 Rime userdb 词库管理；用户词库不再由 Lime 维护平行 JSON 副本。
- `lime-service` 二进制提供 Unix Domain Socket 服务；Windows 构建使用同一帧协议和本地 Named Pipe 入口，服务启动时通过命名互斥体保证单实例。发布启动器应为管道应用 SID ACL。
- IPC 帧具备长度上限和 JSON 解析错误处理；握手版本不匹配直接拒绝。

## 数据与隐私

用户学习词和导入词库通过 librime levers 写入用户数据目录的 `rime_ice.userdb`；导出和清空也由原生 userdb 接口/文件完成。服务默认日志不记录原始前文、preedit、候选分数和 prompt；用户主动打开的请求历史只在内存中保留，不写入磁盘。

## 验证

已运行并通过：

```powershell
cargo fmt --all
cargo check --workspace
cargo test --workspace
```

另外使用按 `resources/runtime/README.md` 准备的本地 llama.cpp 运行时和未入库 GGUF 执行真实评分冒烟；测试环境可通过
`LIME_LLAMA_BACKEND` 选择 `cuda`、`cpu` 或 `auto`，而服务配置的默认后端是 `cuda`：

```powershell
$env:LIME_LLAMA_RUNTIME_DIR = "target/llama-runtime/cuda"
$env:LIME_LLAMA_TEST_MODEL = "resources/models/<verified-model>.gguf"
$env:LIME_LLAMA_BACKEND = "cuda"
cargo test -p lime-core --lib llama::tests::configured_runtime_scores_real_logits -- --ignored
```

模型文件不进入 Git；也可以将 `LIME_LLAMA_TEST_MODEL` 设置为已校验 GGUF 的绝对路径。

测试覆盖默认配置与原子更新、候选召回/学习、无模型 Rime 顺序保持、代际失效、服务输入响应、协议序列化契约，以及真实 GGUF tokenizer/decode/logits 的链式 logprob 与求和不变量。

本地启动（Unix 调试通道）：

```powershell
cargo run -p lime-core --bin lime-service
```

默认 socket 为 `/tmp/lime-core.sock`，可通过 `LIME_SOCKET` 和 `LIME_DATA_DIR` 覆盖；Windows 使用 `LIME_PIPE` 覆盖 Named Pipe 名称。

## 资源边界

仓库只提交雾凇资源说明、许可证、Weasel 主题和固定版本清单（见 `resources/README.md`），不提交大型上游词库、librime DLL、雾凇编译产物、llama.cpp DLL 或 GGUF 模型。发布构建按固定版本清单将官方 librime、雾凇 `full_compiled.zip`、llama.cpp CUDA 13.3 和 CPU runtime 放入安装包；方案选择和候选生成均交给原生 librime。
