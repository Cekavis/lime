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
| D015 | Windows 默认 CUDA，随包提供 CPU fallback；后端由配置选择 |
| D016 | 敏感控件不做特殊处理 |
| D017 | GGUF 文件本身即模型输入，不要求 manifest |
| D018 | 同版本组件，不做旧客户端兼容；握手失败直接拒绝 |
| D019 | Phase 0 只提交工程骨架、契约、资源边界和质量门禁；实验资产不加入生产 workspace |
| D020 | Phase 2 候选窗采用 TSF 进程内原生 Win32 popup；Tauri 不参与实时输入路径 |
| D021 | Phase 3 Tauri 管理窗口只通过 protocol v1 管理 IPC 操作 Rust 服务；配置由 Rust 服务持有，词库持久化交给 librime 原生 userdb |
| D022 | Phase 3 配置文件采用版本化 `config.json`，兼容未包裹的旧配置对象；未知版本不迁移写回 |
| D023 | 在正式品牌图标资源加入前，Tauri 构建脚本生成被忽略的最小占位 ICO，避免管理窗口工程无法独立构建 |
| D024 | Windows 候选窗口默认复用固定版本的 WeaselUI；Lime 保留 TSF/IPC/分页/提交状态机，并通过 UI sidecar 显示前文；WeaselUI 以 GPLv3 源码快照随发布物归档 |
| D025 | 未确认拼音只作为宿主文本框中的 TSF composition 存在；候选窗不重复显示 preedit，只显示前文 auxiliary 行和候选；取消必须先清空组合范围再结束组合 |
| D026 | Windows 候选窗使用屏幕坐标跟随宿主输入光标；空格/数字键提交候选，Enter 始终提交当前英文 preedit 原文 |
| D027 | librime `RimeStartMaintenance(false)` 返回 false 表示没有待处理部署任务时，仍继续创建会话；只有会话/方案初始化失败才报告 Rime 初始化错误 |
| D028 | Windows NSIS 安装包使用 ZLIB 压缩，优先降低 CUDA runtime 大型 DLL 的安装解压时间 |
| D029 | LLM 只接收前 `llm_rerank_count` 个 Rime 候选中、经 librime 候选预览确认已消费完整输入的候选；`llm_effective_count` 限制从该池置顶的数量，未完整候选保留 Rime 原顺序 |
| D030 | Windows TSF 按内置 Rime `ascii_composer` 实现中英模式：左 Shift 无修饰短按切换并提交原始组合串，右 Shift 和 Shift+Space 不切换；英文模式由宿主直接处理半角输入，中文模式使用全角标点 |
| D031 | 最近一次成功激活的模型路径独立于预设持久化；服务启动后在后台尽力自动恢复，恢复期间报告 `reloading`，卸载模型清除记录，恢复失败不阻止 Rime/服务启动 |
| D032 | Tauri 管理窗口只展示用户行动所需的状态与摘要；前台管理数据采用合并轮询和代际丢弃，保护编辑表单、历史详情及预设交互不被刷新打断 |
| D033 | 中文模式的独立全角符号使用短生命周期 TSF composition 提交，不在按键回调中直接调用 `ITfInsertAtSelection`；所有 IME 提交共用已验证的 composition 生命周期，避免 Chromium/WebView2 文本上下文重入崩溃 |
| D034 | Tauri 管理窗口的操作反馈统一使用可关闭、4 秒后自动消失的右下角 toast；服务不可用只通过页头 indicator 展示，不重复弹出连接错误 |

## 后续可演进

- macOS 输入法扩展的具体宿主技术与候选 UI。
- 其他 GPU 后端（Vulkan/ROCm 等）和跨平台硬件策略。
- 模糊音及更丰富的 Rime 扩展。
- 远期是否增加 LLM 生成候选的独立协议版本。
- 是否在仓库治理和许可证审查完成后，将 `third_party/weasel-ui` 迁移为 Lime 组织下的 fork + 固定 commit submodule。
