# 验证与发布

## 本地验证层级

1. 文档/配置：schema 校验、默认值和迁移测试。
2. Rust 核心：单元测试、Rime smoke test、llama.cpp 评分回归、IPC 协议测试。
3. Windows：TSF 注册、前文读取、候选定位、提交、断线透传和高 DPI 测试。
4. 安装：当前用户无管理员权限安装、升级保留用户数据、卸载清理。

## 回归重点

- Rime 候选顺序在 LLM 关闭时完全保持。
- LLM 只对前 `llm_rerank_count` 项评分，前 `llm_effective_count` 项置顶。
- 重复/越界/非法 LLM 索引不会丢失合法 Rime 候选。
- 旧请求不会覆盖新输入。
- 前文获取失败仍可中文输入，但按空上下文处理。
- Rust 服务不可用时只透传英文/数字/标点。
- 默认日志不包含原始输入。

## GitHub Actions

- 普通 push/PR：格式化、静态检查、单元测试、协议/schema 检查。
- `vMAJOR.MINOR.PATCH` tag：Windows x64 构建、打包、产物校验、Release 草稿。
- GGUF、用户词库和大型 Rime 构建产物不进入 Git；Action 使用固定版本资源或缓存。

## 版本

- SemVer。
- Conventional Commits。
- 破坏性协议或用户数据变更必须升级 major，并更新迁移说明。
