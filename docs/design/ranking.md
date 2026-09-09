# 候选生成与 LLM 排序

## 生产基线

生产实现位于 `crates/lime-core`，由 `RimeEngine` 和 `LlamaRuntime` 负责候选召回、tokenizer 边界检查和批量 logprob 计算：

1. Rime/雾凇拼音根据 `preedit` 召回候选。
2. 平台按需请求 Rime 候选前缀；首个请求至少覆盖当前显示页，启用模型时至少覆盖前 `llm_rerank_count` 个候选。翻页超出已读取范围后，再请求更长的前缀，不预先遍历完整候选列表。
3. 检查前 `llm_rerank_count` 个 Rime 候选的 `commit_text_preview`；预览仍包含未消费输入的候选不送入 LLM，也不从该范围之后补位。
4. ASCII 英文候选只有在 `commit_text` 与原始 `preedit` 完全相等时才进入 LLM；其他英文候选保留在 Rime 原始顺序中，但不参与评分。
5. `preceding_text` 为空时跳过 LLM，直接返回 Rime 原始顺序；该路径不依赖已加载的模型运行时。
6. 对 `preceding_text + candidate_text` 做 tokenizer 边界验证，并在诊断行记录 `mismatch`。
7. 使用 llama.cpp backend sampler 在 GPU 上完成 softmax、目标 token gather 和 logprob 计算；主机只读取目标 token 的紧凑结果，不回传完整 vocabulary logits。运行时从本地打包目录或显式环境路径加载，不在服务运行期间下载 native code。
8. 边界不匹配候选将 `tokenize(candidate_text)` 得到的 token 逐个追加到上文，并与正常候选共用
   候选批量 decode 路径计算 logprob；边界不匹配只保留为诊断标记，不再改变评分路径。
9. LLM 只返回完整候选的索引排序，不生成新词、不修改提交文本。

拼音用于 Rime 召回，不直接写入 LLM prompt。

## 三个候选设置

| 设置 | 默认 | 作用 |
|---|---:|---|
| `page_size` | 9 | 前端每页显示数量，仅影响候选 UI |
| `llm_rerank_count` | 32 | 检查并尝试送入 LLM 的 Rime 候选前缀长度；其中未完整消费输入的候选会被排除 |
| `llm_effective_count` | 3 | 从完整候选的 LLM 排序中置顶采纳的候选数量 |

最终顺序：

```text
complete_pool = candidates_whose_preview_consumes_all_input(first_N_rime_candidates)
llm_pool = complete_pool - english_candidates_unless_commit_equals_preedit
final = llm_top_k(llm_pool) + rime_candidates_without(llm_top_k)
```

未完整消费输入的候选以及被英文策略排除的候选不会被 LLM 提升，但仍保留在最终候选列表中。英文放行条件是原始字符串的严格相等比较，不忽略大小写、空格或连字符。其余候选严格保持 Rime 原始顺序。重复、越界或无法解析的索引丢弃；完整候选或有效结果少于 K 时不补造候选。

## 时序与降级

- 每次输入请求同步完成 Rime 召回和可用的 LLM 排序后才返回最终候选。
- 没有上文时不执行 LLM 排序，候选顺序严格采用 Rime 返回值。
- LLM 失败、模型忙、输出非法或内存不足：保持 Rime-only 顺序。
- 并发请求通过 generation 检查丢弃旧结果，旧结果不得覆盖较新的输入。
- Rust 服务崩溃时不能得到 Rime 候选，TSF 直接英文/数字/标点透传。

## 上下文设置

- `preceding_text_char_limit` 默认 128，可在设置中修改。
- `context_preview_char_limit` 默认 32，可在设置中修改。
- `llm_context_token_limit` 默认 1024，也可在设置中修改；llama.cpp 的 batch、micro-batch 和输出
  容量随该值配置。模型加载时 `n_seq_max` 与 `llm_rerank_count` 相同（默认均为 32），每个候选
  使用一个 zero-based sequence；候选数超过该容量时拆分为多个外层 batch。修改上下文、后端或
  重排候选检查范围后，需要下一次受控模型重载才能更新 native context 参数。
- 平台层先按字符裁剪，Rust/llama.cpp 再按 token 后缀截断。

## llama.cpp 后端

- `llm_backend` 默认值为 `cuda`，可切换为 `cpu`。
- `cuda` 会先加载安装包 `llama/cuda/` 下的 CUDA 13.3 runtime，并验证至少一个可用
  CUDA device；DLL、驱动或设备初始化失败时尝试 `llama/cpu/`。
- `cpu` 直接加载 `llama/cpu/`，不依赖 CUDA 驱动。
- 两种后端都使用同一 GGUF、tokenizer、batch decode 和 backend sampler logprob 路径；
  后端只改变 llama.cpp 的 native device 分配，不改变候选排序契约。运行时必须包含
  `llama.cpp-b10743-output-reorder.patch` 生成的 `llama.dll`，否则不得宣称支持紧凑结果路径。

## 诊断行

管理接口的 `CandidateDiagnostic` 同时保留 Rime 原始顺序、LLM 排序顺序、最终展示顺序、聚合/逐 token logprob 和边界 mismatch 标记。诊断只由测试页和历史详情页主动读取，不进入原生候选窗口。

历史记录在实际执行 LLM scorer 时额外保存 `LlmPerformance`：`total_ms` 是 scorer 的总 wall time，
并保留 tokenization、native decode 和紧凑 Logprob 结果读取阶段，同时记录送入候选数、目标 token 数、
解码批次、边界不匹配数、上下文 token 数、decode 输入行数和紧凑 Logprob 结果行数。`decode_ms` 统计 native
decode 调用；兼容字段 `logits_ms` 只统计 decode 返回后的同步和结果读取，不包含 `decode_ms`。没有进入 scorer
的请求不写入该快照。

## 模型

- 模型输入为用户导入的单个 GGUF 文件；不要求额外 manifest。
- 开发模型由 `LIME_LLAMA_TEST_MODEL` 指定；模型文件不进入 Git，使用用户数据目录或绝对路径，不放在仓库资源目录。
- 模型切换通过服务受控重载；同一时间只激活一个模型。多个命名预设由服务持久化到 `model-presets.json`，可列出、保存、删除和切换；服务同时记录最近一次成功激活的模型路径并在下次启动时自动恢复，切换失败不会替换当前模型。
- Windows 首期默认 CUDA，并随安装包提供 CPU 回退；未安装可用模型时仍保持 Rime-only。
