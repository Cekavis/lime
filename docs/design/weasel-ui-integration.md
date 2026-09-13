# WeaselUI 集成

Windows 候选窗口由 TSF 原生层负责。Lime 只复用 vendored `third_party/weasel-ui` 的视觉组件，输入状态、Rime 会话、分页、提交和 IPC 仍由 Lime 自己管理。

首个 composition 更新前，Lime 按小狼毫的时序通过 `ITfUIElementMgr::BeginUIElement` 注册候选 UI，并保持候选列表的单页契约。这样搜索框类宿主会在与小狼毫相同的生命周期位置决定是否接管候选渲染；若宿主返回 `pbShow=FALSE`，候选列表由宿主的集成 UI 绘制，Weasel 主题不参与该分支。

当前边界：

- Windows TSF 只使用 vendored WeaselUI；`third_party/weasel-ui` 缺失时 CMake 直接失败，不保留第二套候选窗口实现。
- WeaselUI 只接收候选快照、服务状态和输入位置，不读取 TSF context，不直接提交文本或访问 Rust IPC。
- 前文预览作为视觉层的辅助信息显示；未确认拼音继续由宿主文本控件渲染。
- `third_party/weasel-ui` 保留固定源码快照、GPLv3 许可证和上游提交信息，发布包同时提供对应许可证文本。

资源与主题：

- 内置 `third_party/rime/weasel.yaml` 提供默认样式；发布时由脚本复制到安装包的 `rime/` 资源目录。
- 用户配置按安装目录、用户 Rime 目录和 `LIME_WEASEL_YAML` 覆盖，未知字段使用 Weasel 默认值。
- 普通桌面进程直接读取用户 Rime 目录；Start/Search 和 Microsoft Store 等受限宿主无法读取该目录时，TSF 通过 Lime 桌面服务取得同一组主题文本，再由同一个解析器应用。单个覆盖文件不可读只会跳过该层，不会让整个样式回退到默认的竖排黑边框。
- TSF 进程只解析候选窗口需要的样式字段，不把主题文件路径传给 WeaselUI。

后续改进保留为独立工作：

- 将 TSF 中的 WeaselUI 适配、候选状态机和 IPC 客户端拆成独立模块。
- 已通过 `third_party/` 快照、许可证和 manifest 固定来源；后续只在升级 WeaselUI 时更新快照并重新验收。
- 在 Windows 实机上验证高 DPI、多显示器、鼠标滚轮、失焦、提交和宿主兼容性。
