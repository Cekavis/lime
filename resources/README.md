# 资源目录

发布包包含可再分发的雾凇拼音文本资源；仓库不提交大型上游词库、GGUF、librime 构建产物或用户词库。

- `rime/`：提交资源说明、许可证和 Weasel 主题；完整 schema、词库、Lua、符号与 OpenCC 资源由发布脚本从雾凇拼音 `2026.06.30` `full_compiled.zip` 下载并原样暂存，默认方案为 `rime_ice`。
- `runtime/`：固定版本的官方 librime、雾凇和 llama.cpp 发布包清单，以及下载/校验说明；llama.cpp 的 CUDA（默认）与 CPU 运行时由发布脚本分别暂存到安装包的 `llama/cuda/` 和 `llama/cpu/` 目录。
- `models/`：用户手动导入的 GGUF 模型位置说明；模型文件不进入 Git。
