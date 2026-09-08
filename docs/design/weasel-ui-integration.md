# WeaselUI 集成

Windows 候选窗口由 TSF 原生层负责。Lime 只复用 vendored `third_party/weasel-ui` 的视觉组件，输入状态、Rime 会话、分页、提交和 IPC 仍由 Lime 自己管理。

当前边界：

- CMake 默认启用 WeaselUI；`LIME_WITH_WEASEL_UI=OFF` 保留内置 Win32 回退窗口。
- WeaselUI 只接收候选快照、服务状态和输入位置，不读取 TSF context，不直接提交文本或访问 Rust IPC。
- 前文预览作为视觉层的辅助信息显示；未确认拼音继续由宿主文本控件渲染。
- `third_party/weasel-ui` 保留固定源码快照、GPLv3 许可证和上游提交信息，发布包同时提供对应许可证文本。

资源与主题：

- 内置 `third_party/rime/weasel.yaml` 提供默认样式；发布时由脚本复制到安装包的 `rime/` 资源目录。
- 用户配置按安装目录、用户 Rime 目录和 `LIME_WEASEL_YAML` 覆盖，未知字段使用 Weasel 默认值。
- TSF 进程只解析候选窗口需要的样式字段，不把主题文件路径传给 WeaselUI。

后续改进保留为独立工作：

- 将 TSF 中的 WeaselUI 适配、候选状态机和 IPC 客户端拆成独立模块。
- 已通过 `third_party/` 快照、许可证和 manifest 固定来源；后续只在升级 WeaselUI 时更新快照并重新验收。
- 在 Windows 实机上验证高 DPI、多显示器、鼠标滚轮、失焦、提交和宿主兼容性。
