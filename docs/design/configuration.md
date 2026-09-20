# 配置与数据目录

## 配置所有权

Rust 核心服务是唯一配置源。Tauri 通过 IPC 读写，TSF 只获取输入相关只读快照。

## 用户设置

```text
rime_schema                  = rime_ice
preceding_text_char_limit   = 128
context_preview_char_limit  = 32
page_size                   = 9
llm_rerank_count            = 32
llm_effective_count         = 3
llm_context_token_limit     = 1024
llm_inference_count_limit   = 1
llm_ignore_emoji            = true
llm_backend                 = cuda
```

`rime_schema` 默认使用雾凇全拼 `rime_ice`；服务启动前也可用 `LIME_RIME_SCHEMA` 选择
安装包内随官方雾凇发布包提供、且当前 Windows 运行时支持的双拼方案。方案资源不由 Lime
解析或改写；上游归档中的其他平台专用文件仍原样保留，但不在 Windows 设置中冒充可用能力。

设置写入后立即成为配置源；Rime 相关设置实时生效。模型已加载时，管理界面保存模型后端、
上下文 token 上限、推理次数上限或重排候选检查范围，会立即重新加载当前模型，使 llama.cpp native 参数生效。
`llm_ignore_emoji` 只影响送入 LLM 的候选池，不影响 llama.cpp native 参数，切换该设置不重新加载模型。
所有设置使用范围校验，非法值拒绝写入。

`llm_rerank_count` 限制每次检查的 Rime 候选前缀长度；Rust 核心通过 librime
候选预览判断其中哪些候选消费了全部输入，仍有剩余拼音的候选不送入模型，也不由
该前缀之后的候选补位。ASCII 英文候选还必须与原始 `preedit` 完全相等，否则只保留在
Rime 顺序中，不送入模型。

不含汉字或 ASCII 英文字母的非 Emoji 候选（如纯数字 `1`、希腊字母 `δ`、纯符号）
固定排除出 LLM 候选池，仍保留在 Rime 顺序中；该规则无需配置，不会排除含汉字的混合词。

`llm_ignore_emoji` 默认开启；开启时，提交文本包含 Emoji code point 的候选不会送入 LLM，混合中文和 Emoji 的候选也会被排除，但这些候选仍保留在最终 Rime 顺序中，不补充其他候选。关闭后恢复原有候选池。

`llm_inference_count_limit` 限制每次输入可使用的候选续写推理批次数，范围为 1 到 32，默认值为 1。
共同上文的推理不计入此额度；从共同起点开始的评分后缀仅含 1 个 token 时，只读取共同上文结果，
也不计入额度。发生分词边界回退时，后缀长度包括需要重算的上文尾部。
超过额度的较长候选保留在 Rime 顺序中，但不参与 LLM 排序。
`llm_effective_count` 表示从这些完整候选的模型排序中实际置顶的数量，且不得大于
`llm_rerank_count`。默认分别为 32 和 3。

服务启动时 librime 可能已经完成资源部署。此时 `RimeStartMaintenance(false)` 返回 false
只表示没有待处理维护任务，不代表 Rime 初始化失败；服务仍会创建会话并校验当前方案。

`llm_backend` 只接受 `cuda` 或 `cpu`，默认值为 `cuda`。`cuda` 表示优先加载打包的 CUDA
llama.cpp CUDA 13.3 runtime；CUDA DLL、驱动或设备初始化失败时，服务按显式降级策略尝试同包的 CPU
 runtime。选择 `cpu` 时不会尝试 CUDA。

Rust 服务将配置持久化到用户数据目录的 `config.json`，格式为 `{ "version": 1, "config": { ... } }`，写入采用临时文件后原子替换。未知版本或非法配置回退到默认值，不覆盖现有文件。
旧版 `config.json` 缺少 `llm_ignore_emoji` 时按默认值 `true` 读取。

## 数据分层

- 内置资源：安装目录 `rime/` 下固定版本的雾凇拼音 schema/词库、符号和 OpenCC 资源；这些资源只读，不进入用户词库导出。
- 用户数据：Rime userdb、自定义词组、模型文件、配置和日志，全部位于当前用户应用数据目录。
- 用户数据不被升级覆盖；内置资源升级通过独立版本目录或原子替换完成。

## 模型管理

- 用户手动导入 GGUF，不自动下载、不联网。
- Windows 发布包同时包含 `llama/cuda/`（CUDA 13.3）与 `llama/cpu/`；默认后端为 CUDA，CPU 作为可选
  后端和 CUDA 初始化失败时的回退路径。
- 记录路径、文件大小、SHA-256 和加载状态；不要求 manifest。
- 导入新模型失败时保留当前模型；没有可用模型时进入 Rime-only。
- 同时只加载一个模型，避免内存叠加。

## 用户词库

- 首期启用 Rime 原生 userdb 和用户词组学习。
- 提供清空、导入、导出；导入先校验，失败不能覆盖现有数据。
- 清空需要二次确认。

## 隐私日志

- 默认日志不保存原始前文、preedit、候选或完整 prompt；管理窗口“请求历史”仅在用户主动查看时显示，并只保存在服务内存中。
- 请求历史可由用户在管理窗口清空，服务重启后自动清空。
- 默认记录服务状态、模型/Rime 错误、IPC 错误和失败类型。
- 完整调试日志必须显式 opt-in，并在 UI 明确提示可能包含输入内容。
- 日志轮转、大小上限和目录由 Rust 服务统一管理。
