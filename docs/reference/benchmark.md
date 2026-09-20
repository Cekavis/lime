# Lime 输入法 benchmark

这个 benchmark 是一个离线、可复现的“给定上文 + 给定拼音，LLM 排序第一候选是否等于标注目标”测试。核心实现位于 `crates/lime-benchmark`，只依赖 `serde`、`serde_json` 和 `sha2`，不调用 `lime-core`、IPC 或 Tauri。管理窗口可以把当前 Rime/模型配置适配成 `Prediction`，再交给这个 crate 做统一校验和统计。

## 数据与输入形式

内置数据集 `lime-daily-v1` 有 48 个原创例子，分成 `chat`、`search`、`prompt`、`article` 四类，每类 12 例。每例保存非空上文、目标词和人工核对的无声调拼音音节。例如：

```json
{
  "id": "prompt-006",
  "category": "prompt",
  "context": "列出三个可行的",
  "expected": "方案",
  "syllables": ["fang", "an"]
}
```

支持两种 `InputMode`：

- `full`：拼音音节直接连接；后续音节以 `a`、`e`、`o` 开头时，在边界前加撇号消除歧义。因此 `方案` 是 `fang'an`，`文案` 是 `wen'an`，而 `火锅` 是 `huoguo`。
- `initials`：每个音节取首字母。因此 `方案` 是 `fa`。`ü` 使用 Rime 常用的 `v` 表示，例如 `旅游` 的音节是 `lv`、`you`，全拼 preedit 为 `lvyou`。

数据集的 JSON 字段和报告字段都使用 snake_case。`validate_dataset` 会拒绝空上文、重复 ID、非中文目标词、拼音音节数量不匹配、非小写 `a-z` 音节、超过 5000 例、超过 8 MiB 的规范化 JSON、超过 4096 字符的上文或超过 32 字符的目标词；解析失败不会静默丢弃例子。

## 评测流程

1. 读取当前服务配置和模型快照；运行记录保存配置快照、配置 revision、模型名称和模型文件哈希。比较结果前仍应固定 Rime schema 与词库/用户词库快照。
2. 对每个 case 和所选输入模式生成 `preedit`，把该 preedit 与 case 的 `context` 交给当前 Lime 服务。
3. 只记录 LLM 排序第一位的最终 commit text，包装成 `Prediction { top1, error, elapsed_ms }`，再调用 `observe`。
4. 所有观察值交给 `build_report`。它会检查 case、模式、上下文、目标词和 preedit 是否与数据集一致，并重新计算 `correct`，不信任调用方传入的布尔值。
5. 报告按每个类别、每种模式以及总体生成摘要，同时保留每个例子的上下文、preedit、目标、top1、错误和耗时，管理窗口可用于展示失败例子。

正确率是严格的字面比较：`error` 存在时一定未命中；`top1` 缺失时记为 `no_prediction`；`top1` 只要和 `expected` 有任何字符差异就未命中。`total` 是该类别在数据集中的例子数，`completed` 是已经收到的观察数，所以空结果和服务错误都留在分母中。只有每个 case × mode 都有且仅有一个观察值时，报告 `complete` 才为 `true`；未完成报告的所有 `accuracy` 都是 `null`，避免把半次运行误当成可比较的分数。

`dataset_sha256` 是对 `Dataset` 使用 `serde_json::to_vec` 得到的无空白规范化 JSON 的 SHA-256，包含 ID、名字、版本和全部内容。相同名字但内容不同的数据集会得到不同指纹。

## 可比性与隐私边界

首期只约定 Rime `rime_ice` 全拼方案（同时测试其全拼和首字母 preedit）。要比较模型或配置，必须固定 Rime schema、词库和用户词库快照、模型文件、模型提示词/排序参数、Rime 部署状态和 Lime 版本；任一项发生变化，都应新建一份报告。用户词库会显著影响候选顺序，因此不能把不同快照的结果直接横向比较。

内置语料是仓库中的原创短句，不来自用户输入历史。自定义数据只在本机读取和导出；benchmark 不学习、不写入输入历史，也不要求上传上文、preedit、候选或模型输出。完整的原文观察值只应在用户明确导出报告时保存；常规 Lime 日志仍不记录这些字段。

## 局限

48 例适合做开发回归和配置对比，不足以代表所有中文输入场景。四个类别是便于诊断的人工切片，不是人口或应用流量的统计样本。指标只看 LLM top1 的严格 commit text，不衡量 top-k、候选排序质量、用户选择成本、长上下文退化或端到端延迟分布；文章类例子也不能替代大规模真实键盘日志。全拼和简拼只取本 crate 生成的两种 preedit，暂不覆盖双拼、模糊音、声调、英文混输或数字符号。

## 相关工作与来源

这些来源用于确定测试维度和限制，不复制其受版权保护的语料：

- Tan 等，**Exploring and Adapting Chinese GPT to Pinyin Input Method**，ACL 2022，原始论文：https://arxiv.org/abs/2203.00249 。论文报告了 perfect pinyin 与 abbreviated pinyin 的差异，并构建了覆盖 15 个领域、约 270K 实例的评测数据；Lime 采用更小的原创开发集，保留按领域切片和分别测全拼/简拼的思路。
- Si 等，**READIN: A Chinese Multi-Task Benchmark with Realistic and Diverse Input Noises**，ACL 2023，原始论文：https://arxiv.org/abs/2302.07324 ，作者公开仓库：https://github.com/thunlp/READIN 。该工作让标注者用常见输入法重新输入测试内容，强调真实输入噪声和多样输入法；Lime 借鉴“真实输入路径应单独评测”的原则，但不下载或复用其数据。
- 雾凇拼音（Rime Ice）官方仓库：https://github.com/iDvel/rime-ice 。仓库列出 `rime_ice` 全拼方案、词库和部署方式；Lime benchmark 的可比性约束以固定该方案及其词库快照为前提。

## 开发检查

将 crate 加入 workspace 后运行：

```powershell
cargo test -p lime-benchmark
cargo fmt --all -- --check
```

`builtin_dataset()` 可用于管理窗口提供默认语料；导入自定义 JSON 时先调用 `parse_dataset`，再将每次服务返回包装为 `Prediction`。`Report` 可直接序列化为 JSON 供管理窗口的独立 benchmark 页面展示。
