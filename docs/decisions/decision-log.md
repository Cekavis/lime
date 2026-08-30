# 决策记录

## 已确认

| 编号 | 决策 |
|---|---|
| D001 | 三层分离：Windows TSF C++、Rust 核心服务、Tauri 管理 UI |
| D002 | 只使用本地 llama.cpp/GGUF，不使用远程模型 |
| D003 | Windows Named Pipe；macOS Unix Domain Socket；当前用户专属 |
| D004 | 前文预览默认 32 字符，实际前文窗口默认 128 字符，均可配置 |
| D005 | 基础 Rime 候选立即显示，LLM 异步重排，可取消并降级 |
| D006 | 候选项只含 `display_text` 和 `commit_text` |
| D007 | `page_size=9`、`llm_rerank_count=32`、`llm_effective_count=3` |
| D008 | LLM 只重排，不生成候选、不改写提交文本 |
| D009 | Rust 服务按需启动、单实例常驻；不可用时英文透传 |
| D010 | 模型缺失/失败时支持 Rime-only |
| D011 | 固定内置 Rime/雾凇资源 + 用户数据覆盖层 |
| D012 | 首期 Windows 10 22H2+ x64；macOS 仅设计 API |
| D013 | 默认不记录原始输入；完整诊断日志显式 opt-in |
| D014 | 启用 Rime 原生 userdb，支持用户词库导入导出 |
| D015 | 首期 CPU-only，GPU 后置 |
| D016 | 敏感控件不做特殊处理 |
| D017 | GGUF 文件本身即模型输入，不要求 manifest |
| D018 | 同版本组件，不做旧客户端兼容；握手失败直接拒绝 |
| D019 | Phase 0 只提交工程骨架、契约、资源边界和质量门禁；实验资产不加入生产 workspace |
| D020 | Phase 2 候选窗采用 TSF 进程内原生 Win32 popup；Tauri 不参与实时输入路径 |
| D021 | Phase 3 Tauri 管理窗口只通过 protocol v1 管理 IPC 操作 Rust 服务；配置、模型加载和词库持久化仍由 Rust 服务负责 |
| D022 | Phase 3 配置文件采用版本化 `config.json`，兼容未包裹的旧配置对象；未知版本不迁移写回 |
| D023 | 在正式品牌图标资源加入前，Tauri 构建脚本生成被忽略的最小占位 ICO，避免管理窗口工程无法独立构建 |

## 后续可演进

- macOS 输入法扩展的具体宿主技术与候选 UI。
- GPU 后端和自动硬件选择。
- 双拼/模糊音及更丰富的 Rime 扩展。
- 远期是否增加 LLM 生成候选的独立协议版本。
