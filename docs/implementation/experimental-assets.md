# 归档资产边界

当前仓库只保留以下历史归档目录，不属于生产运行时：

- `_archive/2026-08-25-ime-context-probe`：Windows TSF/前文读取探针。

该目录不加入根 Cargo workspace，不作为生产 crate 依赖，也不参与实时输入路径。归档目录及其本机生成物由 `.gitignore` 排除；可审计的边界说明维护在本文件中。
