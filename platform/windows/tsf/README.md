# Windows TSF 适配层

这是 Windows 10 22H2+ x64 的 C++ TSF text service。输入路径包含 COM/TSF 生命周期、宿主文本框中的未确认组合串 preedit、光标前文读取、Rust Named Pipe v1 客户端、原生候选 popup、分页/选择/提交，以及连接失败时的英文/数字/标点透传。实时输入路径不依赖 Tauri 管理窗口。

服务管道默认使用 `\\.\pipe\lime-core-v1`，可通过 `LIME_PIPE` 覆盖。若设置 `LIME_SERVICE_PATH`，首次连接失败时 TSF 会按需启动该本地服务并重试。

TSF 根据服务状态中的 `rime_schema` 处理输入；通用方案路径按 Shift/CapsLock 保留字母大小写，
并传递反引号和单引号编码，
候选通过数字键或空格提交；候选只覆盖前缀时，已选文字上屏，剩余拼音保留为新的组合串并继续召回候选，行为对齐小狼毫。回车则提交当前输入的英文原文；中文模式下独立空格始终提交半角 U+0020，其他标点使用雾凇拼音 `half_shape` 的首选映射。不会在 TSF 中伪造 `t9` 的数字处理；上游 `t9.schema`
依赖特定运行时提供的 `t9_processor`，当前官方 Windows librime DLL 未导出该处理器，
因此 Windows 首期不把 t9 列为可选方案，但发布包仍原样保留上游 t9 文件。

## 构建

需要：Visual Studio 2022 的 “Desktop development with C++” 工作负载（MSVC x64/x86 工具集、Windows 10/11 SDK）以及 CMake 3.20+。MSBuild/SDK 本身不足以直接配置本目录的 CMake 工程。

在 Windows Developer PowerShell 中：

```powershell
cmake -S platform/windows/tsf -B out/windows-x64/cmake/tsf -A x64
cmake --build out/windows-x64/cmake/tsf --config Release
```

TSF DLL 和静态链接进来的 WeaselUI 使用 MSVC 静态运行库：Release 为 `/MT`，Debug 为 `/MTd`。TSF 会被不同宿主进程以内嵌 COM 服务器加载，不能依赖宿主私带的 `MSVCP140.dll`/`VCRUNTIME140.dll`；修改 CMake 时必须保留 `MSVC_RUNTIME_LIBRARY` 设置。

非 Windows 主机上配置会明确失败；本目录不携带 SDK、librime 或任何预编译二进制。

发布安装器使用 `perMachine`，以管理员权限将 TSF profile 注册到系统，并在当前用户 profile 下显式启用 Lime。服务端 Named Pipe 采用兼容优先 ACL，允许开始菜单、系统设置等 packaged/AppContainer 宿主连接；当前版本不做调用方身份校验或用户隔离。卸载时会撤销 profile 与 COM 注册。

候选窗口由独立 Win32 UI 线程维护；TSF 在只读 edit session 中获取组合串起点的 `GetTextExt` 屏幕矩形（无可用 layout 时回退到 GUI caret），再按小狼毫的输入位置规则在下方留出 6px 间距。候选窗口只使用固定版本的 WeaselUI（GPLv3），复用 Weasel 的布局、DPI、字体、颜色、圆角、阴影和自定义主题语义；项目不再保留第二套内置绘制器。宿主编辑器负责显示未确认拼音，候选窗不再绘制第二行拼音。TSF 读取的光标前文通过 WeaselUI auxiliary row 显示在候选区域上方，不改变 Weasel 的 Context 序列化布局。安装包同时携带 `licenses/WeaselUI-GPL-3.0.txt` 和对应源码快照。

`OnTestKeyDown` 只做轻量探测，不在探测阶段打开 edit session 或请求服务；部分宿主会在探测回调期间持有 TSF 锁，提前读取上下文会让随后的写会话返回 `TF_E_LOCKED`。候选读取和组合更新统一在 `OnKeyDown` 中执行。

TSF 的 `RequestEditSession` 结果以 `phrSession` 输出参数为准，并使用 `TF_ES_READWRITE` 的 ASYNCDONTCARE 调度：宿主允许时同步执行，否则由 TSF 排队。`StartComposition` 即使返回 `S_OK` 也可能通过空的 `ppComposition` 表示宿主拒绝组合；适配层会检查这一点并显示明确错误。

中文模式下的独立标点（空格除外）即使没有现有拼音组合串，也会建立并立即结束一个短生命周期 TSF composition；独立空格同样复用该生命周期但写入半角 U+0020。不在按键回调中直接调用 `ITfInsertAtSelection` 修改宿主 selection，避免 Chromium/WebView2 等文本上下文的重入崩溃。

候选分页支持 PageUp/PageDown、未移位的主键盘 `-`/`=`、小键盘 `-`/`+` 和 Weasel 滚轮。翻页超出已加载的 Rime 候选时只追加 Rime 候选，不重新调用 LLM，也不产生新的历史记录。F1–F12 等功能键由宿主处理，不参与 Lime 输入。Esc 或退格清空最后一个字母时，会先删除 TSF 组合范围再结束组合；取消完成前继续吞键，避免按键进入宿主文本。

`third_party/weasel-ui` 是 Windows TSF 构建的必需依赖；缺少源码时 CMake 直接失败，不再切换到另一套候选窗口。主题加载顺序和用户迁移方式见 [`docs/design/weasel-ui-integration.md`](../../../docs/design/weasel-ui-integration.md)。
