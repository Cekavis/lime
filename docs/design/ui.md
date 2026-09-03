# UI 与 Design Tokens

## UI 分工

### Windows 原生候选窗口

由 TSF C++ 适配层负责状态和交互，默认交给固定版本的 WeaselUI 绘制（缺少 vendored WeaselUI 时可切换内置 Win32 回退）。TSF 组合串由宿主文本控件以未确认文本渲染；候选窗只显示候选、分页状态和可选的前文预览，不再重复绘制拼音行。前文预览默认显示最近 32 个字符，可设置修改或关闭。

候选项只显示必要信息：候选文本、平台生成的编号和可选的紧凑前文预览。不显示排序分数、延迟、模型名称或内部状态。PageUp/PageDown 以及 Rime 默认的未移位 `-`/`=` 可翻页，滚轮由 WeaselUI 转发到同一状态机。

中文/英文模式遵循内置 Rime `ascii_composer`：左 Shift 无修饰短按切换，切换前提交未确认组合串；右 Shift 为 no-op，Shift+Space 不切换。英文模式不建立 TSF composition，字母、数字和半角符号由宿主键盘布局直接提交；中文模式继续显示候选，并使用内置 Rime 全角标点的首选映射（成对引号按焦点和模式边界重置）。

无拼音组合串时的全角符号仍通过一个立即结束的 TSF composition 提交，与候选提交共用同一写入生命周期；按键回调不直接修改宿主 selection，避免 Chromium/WebView2 文本上下文在 `ITfInsertAtSelection` 中重入 Windows 文本输入框架。

候选窗口使用独立的 Win32 UI 线程和消息循环，不依赖宿主程序（包括记事本、QQ）的 TSF 回调线程绘制。窗口采用 Weasel 的每监视器 DPI、圆角、选中态高亮、编号列和状态提示；服务不可用或没有候选时，状态提示明确显示“英文透传”，而不是留下不可见的组合状态。

`OnTestKeyDown` 只负责判断按键归属，不读取上下文、不访问 IPC；候选读取和写入组合串在 `OnKeyDown` 的 edit session 中完成，避免宿主在 probe 回调期间持有锁而造成 `TF_E_LOCKED`。

TSF 写入组合串时以 `RequestEditSession` 的 `phrSession` 结果为准，使用 `TF_ES_READWRITE` 的 ASYNCDONTCARE 调度。`StartComposition` 的 `S_OK` 不代表一定创建了组合，必须同时检查 `ppComposition`；宿主拒绝时显示明确诊断而不是静默透传。取消会先清空组合范围再结束组合，并把 selection 折叠到组合末端；未完成的异步取消会继续吞键，避免 Esc 或后续字母泄漏到宿主。

### Tauri 管理窗口

使用 Tauri 2、Tailwind、shadcn，包含：

- 输入与候选设置
- 输入测试（上文 + 拼音，复用真实 `InputRequest` 路径）
- GGUF 导入/选择
- Rime 用户词库导入/导出/清空
- 请求历史（上文、拼音、Rime 候选、最终候选）
- 服务状态和最小诊断信息

管理界面不参与实时按键和候选替换。

Phase 3 管理窗口通过 Tauri command 调用 Rust 服务的 protocol v1 管理 IPC。设置页覆盖全部用户配置；模型页提交本地 GGUF 路径并显示服务返回的文件大小、SHA-256 和加载状态；词库页导入/导出 JSON、清空并显示条目预览；诊断页只显示服务状态、模型状态、词库数量和最近 UI 操作。UI 不直接读写配置、模型或用户词库持久化文件，也不在常规页面暴露协议或配置版本号。

管理窗口轮询服务状态及管理数据，并将重叠请求串行合并；窗口可见性变化、切换页面和手动刷新都会立即更新。历史使用服务端 revision 长轮询通知，记录新增或清空时立即触发刷新，不受窗口后台状态或 WebView 定时器节流影响；历史表格按行复用以保留焦点和鼠标交互。轮询结果带有请求代际，过时结果不会覆盖新操作；设置表单和模型预设操作期间保留用户当前交互，交互结束后再应用延迟快照。

操作成功或失败使用右下角单条 toast 提示：提示可手动关闭，并在 4 秒后自动消失，不占用页面内容区域，也不遮挡页头 indicator。服务不可用不弹出错误提示，用户只通过页头右上角的服务状态 indicator 判断可用性。

## Design tokens

首期建立集中 token 文件，至少覆盖：

- color：background、foreground、muted、accent、destructive、border
- typography：font family、size、weight、line height
- spacing：最小间距阶梯
- radius：基础圆角和控件圆角
- elevation：窗口/弹层阴影
- motion：短过渡时长和 easing

组件只能引用语义 token，不得在页面或组件内写任意颜色、字号、间距、圆角和阴影值。新增组件变体必须先更新 token 与组件规范。

## 视觉原则

- 简洁、低干扰、无多余说明文字。
- 设置项使用清晰标签和原生控件，不重复解释实现细节。
- 状态只显示用户需要采取行动的信息：可用、Rime-only、重载中、服务不可用。
- Figma 可作为视觉确认工具，但最终 token 和组件规范必须落入仓库文档/代码。
