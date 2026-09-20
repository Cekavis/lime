# 候选生成与 LLM 排序

## 生产基线

生产实现位于 `crates/lime-core`，由 `RimeEngine` 和 `LlamaRuntime` 负责候选召回、tokenizer 边界检查和批量 logprob 计算：

1. Rime/雾凇拼音根据 `preedit` 召回候选。
2. 平台按需请求候选页面前缀；服务在模型启用时可额外读取前 `llm_rerank_count` 个 Rime 候选完成排序，但响应只返回请求的前缀。翻页超出已读取范围后，再请求更长的前缀，不预先遍历完整候选列表；这类扩展请求只追加 Rime 候选、不重新调用 LLM，并保持已返回的最终排序前缀。
3. 检查前 `llm_rerank_count` 个 Rime 候选的 `commit_text_preview`；预览仍包含未消费输入的候选不送入 LLM，也不从该范围之后补位。
4. 按 `commit_text` 排除不含汉字或 ASCII 英文字母的非 Emoji 候选，包括纯数字、希腊字母和纯符号（如 `1`、`１２`、`δ`、`∑`）；汉字包含 CJK 扩展/兼容汉字及 `〇`，中数、中英等混合词仍可参与。ASCII 英文候选只有在 `commit_text` 与原始 `preedit` 完全相等时才进入 LLM；其他英文候选保留在 Rime 原始顺序中，但不参与评分。
5. 开启 `llm_ignore_emoji` 时，包含 Emoji code point 的候选也不进入 LLM；混合中文和 Emoji 的候选同样排除，但仍保留在 Rime 原始顺序中。
6. `preceding_text` 为空时跳过 LLM，直接返回 Rime 原始顺序；该路径不依赖已加载的模型运行时。
7. 将每个 `preceding_text + candidate_text` 联合分词，并与单独分词的上文一起求最长共同 token
   前缀，作为整次请求唯一的评分起点。诊断行的 `mismatch` 仍记录各候选是否改变了原始上文边界。
8. 使用 llama.cpp backend sampler 在 GPU 上完成 softmax、目标 token gather 和 logprob 计算；主机只读取目标 token 的紧凑结果，不回传完整 vocabulary logits。运行时从本地打包目录或显式环境路径加载，不在服务运行期间下载 native code。
9. 从共同起点逐 token 评分每条联合分词路径的剩余部分。发生边界回退时，所有候选都重算上文
   尾部，包括没有 `mismatch` 的候选；同一请求的所有批次使用相同起点和相同左截断后的上文。
10. LLM 只返回完整候选的索引排序，不生成新词、不修改提交文本。

拼音用于 Rime 召回，不直接写入 LLM prompt。

例如，上文“一直用的是公司”的 token 为 `[一直][用][的是][公司]`，与“的”联合分词得到
`[一直][用][的是][公司的]`，与“得”联合分词得到 `[一直][用][的是][公司][得]`。共同前缀是
`[一直][用][的是]`；评分目标分别为 `[公司的]` 和 `[公司][得]`。各候选的 logprob 都覆盖
同一文字位置之后的完整联合分词路径，禁止把 `[公司][的]` 作为替代路径，或让没有 mismatch
的候选仍从“公司”之后评分。回退只改变模型内部计算，不修改上文、候选或提交文本。
若共同前缀为空，使用 GGUF 显式声明的 BOS；没有 BOS 时使用显式 EOS 作为文档边界。
不得把运行库默认的普通字符 token 当作 BOS；缺少合法边界 token 时明确报告评分错误。

## 候选设置

| 设置 | 默认 | 作用 |
|---|---:|---|
| `page_size` | 9 | 前端每页显示数量，仅影响候选 UI |
| `llm_rerank_count` | 32 | 检查并尝试送入 LLM 的 Rime 候选前缀长度；其中未完整消费输入的候选会被排除 |
| `llm_effective_count` | 3 | 从完整候选的 LLM 排序中置顶采纳的候选数量 |
| `llm_context_token_limit` | 1024 | 单次 llama.cpp 请求允许的总 token 行数 |
| `llm_inference_count_limit` | 1 | 每次输入允许的候选续写推理批次数；共同上文和单 token 候选不计入 |
| `llm_ignore_emoji` | true | 是否从 LLM 重排候选池排除包含 Emoji code point 的候选 |

最终顺序：

```text
complete_pool = candidates_whose_preview_consumes_all_input(first_N_rime_candidates)
llm_pool = complete_pool - non_emoji_candidates_without_han_or_ascii_letters
llm_pool = llm_pool - english_candidates_unless_commit_equals_preedit
llm_pool = llm_pool - emoji_candidates_when_llm_ignore_emoji
final = llm_top_k(llm_pool) + rime_candidates_without(llm_top_k)
```

未完整消费输入、被文本/英文策略排除或被 Emoji 设置排除的候选不会被 LLM 提升，但仍保留在最终候选列表中；排除候选不会触发其他候选补位。文本筛选也适用于未提供 `preedit` 的重排入口；筛选后候选池为空时跳过 LLM，保留 Rime 原始顺序且不产生 LLM 性能快照。英文放行条件是原始字符串的严格相等比较，不忽略大小写、空格或连字符，也不能绕过数字/符号筛选。Emoji 仍只由 `llm_ignore_emoji` 控制；判断按提交文本中的 code point 进行，因此混合中文和 Emoji 的候选也会被该设置排除。其余候选严格保持 Rime 原始顺序。重复、越界或无法解析的索引丢弃；完整候选或有效结果少于 K 时不补造候选。

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
- 平台层先按字符裁剪，Rust/llama.cpp 再按 token 后缀截断。token 窗口在整次请求中统一选定，
  保留共同前缀的同一后缀，为联合分词后的目标 token 预留空间；不得按外层批次分别裁剪上文。
- `llm_ignore_emoji` 默认开启；它只改变送入 LLM 的候选池，不影响 llama.cpp native 参数，因此切换后不需要重新加载模型。

## 候选批量推理

模型加载后按能力选择两条路径。纯 attention 模型先建立共享上文 KV，再把按 token 数从短到长
排列的候选 continuation prefix 放入 ragged packed tree；每条分支只提交真实 token 行，不做
padding。共享上文和 packed continuation 分别只解码一次。Qwen3.5 等 hybrid/recurrent 模型
保留等长 padding batch，但同样按短候选优先分批。

当前 llama.cpp runtime 对共享 sequence id 的同一 native decode 会让未来分支影响共享根节点
的输出，因此 attention 路径没有把根和未来分支强行合并为一次 native decode；若要做到这个
更严格的形式，需要先修改 llama.cpp 的 sequence/KV 图构建逻辑。候选 continuation 本身仍
保持一次 ragged packed decode，且不引入 padding。
每次输入只在候选续写批次上消耗 `llm_inference_count_limit`；达到上限后剩余候选不再送入模型。
共同上文 decode 和评分后缀仅含 1 个 token 的候选不消耗额度；后缀长度包含回退重算的上文。

每个候选的目标 token 由 Lime 的 GPU sampler 通过 `ggml_get_rows` 直接 gather；结果从 sampler
result tensor 读取，并按 sampler 行保存的目标 token 映射回候选。这个路径读取任意指定 token
的 logprob，不需要回传完整 vocabulary logits，也不依赖 token 是否位于概率 top-k。

Qwen3.5 等混合 recurrent/attention 模型在单 token decode 时使用 autoregressive gated-delta-net
路径，在一个序列包含多个 token 的逻辑 batch 中使用 chunked/fused 路径。两者保持因果语义，
但浮点运算顺序和中间状态归约不同，因此多 token continuation 的 logprob 可能与逐 token
autoregressive 基线有数值差异；共同上文和首个候选 token 的 logprob 应保持一致。若要求逐位
复现 autoregressive 结果，需要在 llama.cpp kernel 中为批量路径实现相同的逐步状态更新，这会
重新引入部分逐步计算成本。

2026-09-09 在 RTX 5070 Ti、CUDA、Qwen3.5 0.8B Q4_K_M 上，用 25 个中文候选和 7 个 emoji
候选（目标 token 总数 54）实测，合并 continuation batch 的稳定耗时约为 17–22 ms，性能诊断
中的 `batch_count` 为 1；此前按 token 深度重复 decode 的同类测试约为 37–60 ms。首次 CUDA
graph 建立包含额外 warmup 成本，不能与稳态请求直接比较。

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
并保留 tokenization、native decode 阶段以及送入候选数、目标 token 数、解码批次、边界不匹配数、
上下文 token 数、decode 输入行数和紧凑 Logprob 结果行数。`decode_ms` 统计 native decode 调用；没有进入
scorer 的请求不写入该快照。

发生回退时，快照额外包含 `boundary_rollback`：`prefix_token_count` 是共同起点之前的原始
上文 token 数（模型窗口左截断之前），`replayed_token_count` 是从该点重算的原始上文 token
数，两者之和等于 `context_token_count`。若原始 token bytes 能精确映射到上文的 UTF-8
边界，同时返回 `prefix_text` 与 `replayed_text`，分别是 Rust scorer 实际收到的有效上文
在起点之前和之后的文本；否则两者都缺省，只给出准确 token 位置。无回退时整个字段缺省。

历史详情页和测试页在候选表格上方显示本次回退位置。例如“一直用的是｜公司”，并说明“公司”
与各候选一起重算；没有精确文本映射时显示从第几个上文 token 开始重算及回退 token 数，不猜测
字符位置。`logprobs`、聚合 `logprob` 和 `target_token_count` 均包含回退后重新评分的上文尾部；
某行的 `mismatch=false` 不表示该行免于共同回退。没有回退信息的旧载荷不显示该说明。

## 模型

- 模型输入为用户导入的单个 GGUF 文件；不要求额外 manifest。
- 开发模型由 `LIME_LLAMA_TEST_MODEL` 指定；模型文件不进入 Git，使用用户数据目录或绝对路径，不放在仓库资源目录。
- 模型切换通过服务受控重载；同一时间只激活一个模型。多个命名预设由服务持久化到 `model-presets.json`，可列出、保存、删除和切换；服务同时记录最近一次成功激活的模型路径并在下次启动时自动恢复，切换失败不会替换当前模型。
- 目前只接受 causal decoder。纯 attention 模型使用 packed tree，recurrent/hybrid 模型使用 padding
  路径；encoder、embedding、diffusion 等无法归入这两条路径的模型在加载时明确报告为不支持。
- Windows 首期默认 CUDA，并随安装包提供 CPU 回退；未安装可用模型时仍保持 Rime-only。
