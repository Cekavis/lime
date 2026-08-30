# 实现计划

## Phase 0：工程骨架

状态：已完成（2026-08-30）。详细文件与验证记录见 [Phase 0 实现记录](phase-0.md)。

- 初始化 Rust workspace、Tauri 2 管理窗口、Windows C++ TSF 工程。
- 建立资源目录、配置 schema、IPC schema 和统一错误码。
- 将 `_archive/2026-08-25-ime-context-probe` 与 `tools/pinyin-eval` 标记为实验/评测资产，不作为生产 crate 依赖。

## Phase 1：Rust 核心服务

状态：已完成（2026-08-30）。详细文件与验证记录见 [Phase 1 实现记录](phase-1.md)。

- 实现单实例、按需启动、Named Pipe 服务。
- 抽取 `RimeEngine`，复用 pinyin-eval 的 librime 动态加载和用户目录隔离逻辑。
- 抽取 `LlamaRuntime`，首期 CPU-only，支持 GGUF 加载/卸载和 token/logits 评分。
- 实现 Rime-only、模型重排、取消旧代际和配置热更新。
- 实现用户词库学习、导入导出和隐私安全日志。

## Phase 2：Windows TSF

状态：已完成（2026-08-30）。详细文件与验证记录见 [Phase 2 实现记录](phase-2.md)。

- 从 context probe 重组 TSF/COM、前文读取、preedit 状态机和英文透传。
- 实现原生候选窗口、分页、选择、提交和前文预览。
- 实现服务发现、握手、断线重连和 unavailable 透传行为。

## Phase 3：Tauri 管理 UI

- 按 token 规范实现设置、模型、词库和诊断页面。
- 通过 IPC 调用配置服务，不直接读写用户文件。
- 增加设置迁移、错误提示和最小状态展示。

## Phase 4：验收与发布

- Windows 10 22H2+ x64 安装/卸载/升级验证。
- 中文输入、前文获取、Rime-only、LLM 重排和服务崩溃透传验证。
- 生成签名/校验信息，使用版本 tag 触发 GitHub Actions。

## 明确不在首期

- macOS 可用输入法实现
- 远程模型或自动下载
- 双拼、模糊音、脚本扩展
- LLM 自主生成候选
- GPU 加速必达
- 敏感控件特殊处理
