# Lime 项目协作约定

## 项目定位

Lime（Language model IME）是一个本地优先的中文拼音输入法。Windows 首期支持 Windows 10 22H2 及以上的 x64；macOS 首期只完成跨平台核心 API 与平台适配层设计，不承诺可用输入法版本。

## 架构边界

- Windows TSF 适配层使用现代 C++，只负责 TSF/COM 生命周期、按键与组合串、获取光标前文本、原生候选窗口、英文透传和本机 IPC 客户端。
- 跨平台核心服务使用 Rust，负责配置、Rime/雾凇拼音候选生成、llama.cpp/GGUF 推理、LLM 重排、用户词库和日志。
- Tauri 2 + Tailwind + shadcn 主窗口只负责设置、模型管理、Rime 配置、词库管理和诊断，不参与实时按键路径。
- macOS 适配层未来复用 Rust 核心服务协议，不提前实现平台功能。

## 设计原则

- 本地优先：不使用远程模型，不上传输入内容。
- Rime 负责候选召回，LLM 只负责重排，禁止生成候选或改写提交文本。
- 基础候选即时可用，LLM 异步重排；旧请求必须可取消，过期结果不得覆盖新输入。
- Rust 核心服务是配置唯一拥有者；其他组件通过 IPC 访问。
- 服务不可用时没有 Rime fallback，Windows 适配层进入英文/数字/标点透传；模型不可用时仍可运行 Rime-only 模式。
- 默认不记录原始前文、preedit 和候选内容；完整调试日志必须显式 opt-in。
- IPC 使用当前用户专属的 Windows Named Pipe / macOS Unix Domain Socket，不监听网络端口。
- 首期 CPU-only 必须可用，GPU 加速后置。

## UI 约束

- 所有 UI 必须使用集中维护的 design tokens；禁止在组件中随意创建颜色、字号、间距、圆角、阴影或组件变体。
- 优先复用 shadcn 组件和已有变体；新增变体必须先更新 token/组件规范。
- 界面保持简洁，只显示完成任务所必需的文字。
- 输入候选窗口使用平台原生 UI；Tauri 窗口不显示实时输入候选。

## 文档与变更

- 统一入口为 `README.md`，文档索引位于 `docs/README.md`。
- 架构、协议、实现计划、UI/token、决策记录分别维护在 `docs/` 下。
- 每次行为或接口变更必须同步更新相关设计/实现文档和决策记录。
- 使用 Conventional Commits；版本采用 SemVer，Git tag `vMAJOR.MINOR.PATCH` 触发发布构建。
- PR/push 运行检查与测试；版本 tag 运行 Windows x64 构建、打包和产物校验。
- 不提交 GGUF、librime 二进制、用户词库、模型缓存、构建产物和实验输出。

## 验证要求

- 先查找现有测试和构建命令，再运行最窄的相关检查。
- 不通过削弱断言、缩小覆盖范围或跳过检查来制造通过结果。
- 发现与本次改动无关的已有失败时，明确记录，不归因于本次改动。
