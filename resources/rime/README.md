# Rime / 雾凇拼音内置资源

此目录对应雾凇拼音 `2026.06.30` 发布包 `full_compiled.zip`；发布包 SHA-256 为 `ae810e0023f28d3eb45ff98a7c58830dd0b505c2272638a321fa6d5a6f5d5418`。仓库只保留本说明、许可证和 Lime 使用的 Weasel 主题；大型词库、Lua、OpenCC 及编译数据不进入 Git，由 `tools/release/prepare-rime-runtime.ps1` 按 manifest 下载并校验。

这些文件是只读基础资源，不会出现在管理窗口的用户词库列表中，也不会被“清空用户词库”删除。用户学习词和导入词库由 librime 的 `rime_ice.userdb` 管理。

`default.yaml`、全拼/双拼/九键/拆字/英文方案、Lua 扩展、英文与中文词库、OpenCC 映射和 `custom_phrase.txt` 在构建暂存目录中保持发布包内容；运行时由原生 librime 读取这些文件，不在 Lime 内重复解析或改写词库。用户数据另行写入用户目录，不覆盖这里的只读资源。
