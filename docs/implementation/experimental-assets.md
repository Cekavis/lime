# 实验资产边界

Phase 0 明确以下目录只用于实验与离线评测，不属于生产运行时：

- `_archive/2026-08-25-ime-context-probe`：Windows TSF/前文读取探针。
- `tools/pinyin-eval`：Rime/雾凇拼音与 llama.cpp 候选评分评测工具。

两者不加入根 Cargo workspace，不作为生产 crate 依赖，也不参与实时输入路径。由于目录包含本机生成物和大体积二进制，继续由 `.gitignore` 忽略其运行输出；可审计的边界说明维护在本文件中。
