# 候选生成与 LLM 排序

## 生产基线

核心逻辑沿用 `tools/pinyin-eval`：

1. Rime/雾凇拼音根据 `preedit` 召回候选。
2. 检查前 `llm_rerank_count` 个 Rime 候选的 `commit_text_preview`；预览仍包含未消费输入的候选不送入 LLM，也不从该范围之后补位。
3. ASCII 英文候选只有在 `commit_text` 与原始 `preedit` 完全相等时才进入 LLM；其他英文候选保留在 Rime 原始顺序中，但不参与评分。
4. `preceding_text` 为空时跳过 LLM，直接返回 Rime 原始顺序；该路径不依赖已加载的模型运行时。
5. 对 `preceding_text + candidate_text` 做 tokenizer 边界验证，并在诊断行记录 `mismatch`。
6. 使用 llama.cpp 完整 vocabulary logits 计算候选 token 的链式 logprob；运行时从本地打包目录或显式环境路径加载，不在服务运行期间下载 native code。
7. 边界不匹配候选仍沿用逐 token 的 standalone-token 评分语义，但在 tokenization 阶段预先构造每个
   prompt/target 对，随后按统一 token 预算批量 decode；因此不会为每个目标 token 单独清空 KV 和调用
   llama.cpp。
8. LLM 只返回完整候选的索引排序，不生成新词、不修改提交文本。

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

- Rime 候选立即可见。
- LLM 在后台异步重排；新输入取消旧代际。
- 没有上文时不执行 LLM 重排，候选顺序严格采用 Rime 返回值。
- LLM 超时、模型忙、输出非法或内存不足：保持 Rime-only 顺序。
- Rust 服务崩溃时不能得到 Rime 候选，TSF 直接英文/数字/标点透传。

## 上下文设置

- `preceding_text_char_limit` 默认 128，可在设置中修改。
- `context_preview_char_limit` 默认 32，可在设置中修改。
- `llm_context_token_limit` 默认 1024，也可在设置中修改；llama.cpp 的 batch、micro-batch 和输出
  容量随该值配置。sequence slot 保持 33 的安全上限，避免 recurrent 模型按 sequence 数量分配
  过大的状态内存；一次 decode 的输入行数超过该值时仍会拆分为多个外层 batch。
- 平台层先按字符裁剪，Rust/llama.cpp 再按 token 后缀截断。

## llama.cpp 后端

- `llm_backend` 默认值为 `cuda`，可切换为 `cpu`。
- `cuda` 会先加载安装包 `llama/cuda/` 下的 CUDA 13.3 runtime，并验证至少一个可用
  CUDA device；DLL、驱动或设备初始化失败时尝试 `llama/cpu/`。
- `cpu` 直接加载 `llama/cpu/`，不依赖 CUDA 驱动。
- 两种后端都使用同一 GGUF、tokenizer、batch decode 和完整 vocabulary logits 计算路径；
  后端只改变 llama.cpp 的 native device 分配，不改变候选排序契约。

## 诊断行

管理接口的 `CandidateDiagnostic` 同时保留 Rime 原始顺序、LLM 排序顺序、最终展示顺序、聚合/逐 token logprob 和边界 mismatch 标记。诊断只由测试页和历史详情页主动读取，不进入原生候选窗口。

历史记录在实际执行 LLM scorer 时额外保存 `LlmPerformance`：`total_ms` 是 scorer 的总 wall time，
并拆分 tokenization、native decode 和 logits 阶段，同时记录送入候选数、目标 token 数、解码批次、
边界回退数、上下文 token 数、decode 输入行数和 logits 输出行数。没有进入 scorer 的请求不写入该快照。

## 模型

- 模型输入为用户导入的单个 GGUF 文件；不要求额外 manifest。
- 当前开发模型：`tools/pinyin-eval/native/llama/models/qwen3.5-4b-base-q4_k_m/qwen3.5-4b-base-q4_k_m.gguf`。
- 模型切换通过服务受控重载；同一时间只激活一个模型。多个命名预设由服务持久化到 `model-presets.json`，可列出、保存、删除和切换；服务同时记录最近一次成功激活的模型路径并在下次启动时自动恢复，切换失败不会替换当前模型。
- Windows 首期默认 CUDA，并随安装包提供 CPU 回退；未安装可用模型时仍保持 Rime-only。
