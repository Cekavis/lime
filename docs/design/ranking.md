# 候选生成与 LLM 排序

## 生产基线

核心逻辑沿用 `tools/pinyin-eval`：

1. Rime/雾凇拼音根据 `preedit` 召回候选。
2. 过滤不能完整覆盖当前拼音的短候选。
3. 对 `preceding_text + candidate_text` 做 tokenizer 边界验证。
4. 使用 llama.cpp 完整 vocabulary logits 计算候选 token 的链式 logprob。
5. 边界不匹配候选沿用现有逐 token 评分路径。
6. LLM 只返回候选索引排序，不生成新词、不修改提交文本。

拼音用于 Rime 召回，不直接写入 LLM prompt。

## 三个候选设置

| 设置 | 默认 | 作用 |
|---|---:|---|
| `page_size` | 9 | 前端每页显示数量，仅影响候选 UI |
| `llm_rerank_count` | 32 | 送入 LLM 重排的 Rime 候选数量 |
| `llm_effective_count` | 3 | 从 LLM 排序中置顶采纳的候选数量 |

最终顺序：

```text
final = llm_top_k + rime_candidates_without(llm_top_k)
```

其余候选严格保持 Rime 原始顺序。重复、越界或无法解析的索引丢弃；有效结果少于 K 时不补造候选。

## 时序与降级

- Rime 候选立即可见。
- LLM 在后台异步重排；新输入取消旧代际。
- LLM 超时、模型忙、输出非法或内存不足：保持 Rime-only 顺序。
- Rust 服务崩溃时不能得到 Rime 候选，TSF 直接英文/数字/标点透传。

## 上下文设置

- `preceding_text_char_limit` 默认 128，可在设置中修改。
- `context_preview_char_limit` 默认 32，可在设置中修改。
- `llm_context_token_limit` 默认 32，也可在设置中修改。
- 平台层先按字符裁剪，Rust/llama.cpp 再按 token 后缀截断。

## 模型

- 模型输入为用户导入的单个 GGUF 文件；不要求额外 manifest。
- 当前开发模型：`tools/pinyin-eval/native/llama/models/qwen3.5-4b-base-q4_k_m/qwen3.5-4b-base-q4_k_m.gguf`。
- 模型切换通过服务受控重载；同一时间只激活一个模型。
- 首期 CPU-only；GPU 后端以后通过 llama.cpp 适配器增加。
