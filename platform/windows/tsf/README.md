# Windows TSF 适配层（Phase 2）

这是 Windows 10 22H2+ x64 的 C++ TSF text service。输入路径包含 COM/TSF 生命周期、宿主文本框中的未确认组合串 preedit、光标前文读取、Rust Named Pipe v1 客户端、原生候选 popup、分页/选择/提交，以及连接失败时的英文/数字/标点透传。实时输入路径不依赖 Tauri 管理窗口。

服务管道默认使用 `\\.\pipe\lime-core-v1`，可通过 `LIME_PIPE` 覆盖。若设置 `LIME_SERVICE_PATH`，首次连接失败时 TSF 会按需启动该本地服务并重试。

TSF 根据服务状态中的 `rime_schema` 处理输入；通用方案路径按 Shift/CapsLock 保留字母大小写，
并传递反引号和单引号编码，
候选通过数字键或空格提交，回车则提交当前输入的英文原文。不会在 TSF 中伪造 `t9` 的数字处理；上游 `t9.schema`
依赖特定运行时提供的 `t9_processor`，当前官方 Windows librime DLL 未导出该处理器，
因此 Windows 首期不把 t9 列为可选方案，但发布包仍原样保留上游 t9 文件。

## 构建

需要：Visual Studio 2022 的 “Desktop development with C++” 工作负载（MSVC x64/x86 工具集、Windows 10/11 SDK）以及 CMake 3.20+。MSBuild/SDK 本身不足以直接配置本目录的 CMake 工程。

在 Windows Developer PowerShell 中：

```powershell
cmake -S platform/windows/tsf -B build/tsf -A x64
cmake --build build/tsf --config Release
```

非 Windows 主机上配置会明确失败；本目录不携带 SDK、librime 或任何预编译二进制。

发布安装器使用 `perMachine`，以管理员权限将 TSF profile 注册到系统，并在当前用户 profile 下显式启用 Lime；服务进程仍按当前用户 SID 配置 Named Pipe ACL。卸载时会撤销 profile 与 COM 注册。

候选窗口由独立 Win32 UI 线程维护，将宿主提供的客户区光标矩形转换为屏幕坐标后跟随输入光标。默认使用固定版本的 WeaselUI（GPLv3），复用 Weasel 的布局、DPI、字体、颜色、圆角、阴影和自定义主题语义；宿主编辑器负责显示未确认拼音，候选窗不再绘制第二行拼音。TSF 读取的光标前文通过 WeaselUI auxiliary row 显示在候选区域上方，不改变 Weasel 的 Context 序列化布局。安装包同时携带 `licenses/WeaselUI-GPL-3.0.txt` 和对应源码快照。

`OnTestKeyDown` 只做轻量探测，不在探测阶段打开 edit session 或请求服务；部分宿主会在探测回调期间持有 TSF 锁，提前读取上下文会让随后的写会话返回 `TF_E_LOCKED`。候选读取和组合更新统一在 `OnKeyDown` 中执行。

TSF 的 `RequestEditSession` 结果以 `phrSession` 输出参数为准，并使用 `TF_ES_READWRITE` 的 ASYNCDONTCARE 调度：宿主允许时同步执行，否则由 TSF 排队。`StartComposition` 即使返回 `S_OK` 也可能通过空的 `ppComposition` 表示宿主拒绝组合；适配层会检查这一点并显示明确错误。

候选分页支持 PageUp/PageDown、未移位的主键盘 `-`/`=`、小键盘 `-`/`+` 和 Weasel 滚轮。Esc 或退格清空最后一个字母时，会先删除 TSF 组合范围再结束组合；异步取消完成前继续吞键，避免按键进入宿主文本。

若构建环境暂时没有 `third_party/weasel-ui`，可用 `-DLIME_WITH_WEASEL_UI=OFF` 构建内置回退窗口；发布构建默认启用 WeaselUI。主题加载顺序和用户迁移方式见 [`docs/design/weasel-ui-integration.md`](../../../docs/design/weasel-ui-integration.md)。
