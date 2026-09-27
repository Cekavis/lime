# Windows TSF 适配层

这是 Windows 10 22H2+ x64 的 C++ TSF text service。输入路径包含 COM/TSF 生命周期、宿主文本框中的未确认组合串 preedit、光标前文读取、Rust Named Pipe v1 客户端、原生候选 popup、分页/选择/提交，以及连接失败时的英文/数字/标点透传。实时输入路径不依赖 Tauri 管理窗口。

服务管道默认使用 `\\.\pipe\lime-core-v1`，可通过 `LIME_PIPE` 覆盖。若设置 `LIME_SERVICE_PATH`，首次连接失败时 TSF 会按需启动该本地服务并重试。

TSF 根据服务状态中的 `rime_schema` 处理输入；通用方案路径按 Shift/CapsLock 保留字母大小写，
并传递反引号和单引号编码，
候选通过数字键或空格提交；候选只覆盖前缀时，已选文字上屏，剩余拼音保留为新的组合串并继续召回候选，行为对齐小狼毫。服务可用但 Rime 返回空候选时也保留原始组合串，不向宿主透传最后一个字母，直到回车或空格提交。回车则提交当前输入的英文原文；中文模式下独立空格始终提交半角 U+0020，其他标点使用雾凇拼音 `half_shape` 的首选映射。不会在 TSF 中伪造 `t9` 的数字处理；上游 `t9.schema`
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

首个 composition 更新前，候选 UI 按小狼毫的时序注册并保持单页列表契约。搜索框类宿主仍可能在 `BeginUIElement` 返回 `pbShow=FALSE` 后使用自己的集成候选 UI；这时方向、字体和颜色由宿主决定。

组合串使用与小狼毫兼容的 TSF display attribute（点状下划线），并在组合串真正创建后再刷新候选窗口位置；适配层同时监听宿主的 TSF layout change，在首字符完成布局后重新读取组合串末端的位置，避免首个字母沿用上一次位置。读取光标前文优先使用通用 TSF range 移动接口，只有宿主不支持时才回退到 ACP 范围。

Telegram 7.2.x 的输入框是 Qt `QTextEdit`。当 TSF range 返回空前文时，适配层在 edit session 结束后通过独立的 UI Automation MTA 查询当前获得焦点的 Qt 编辑控件，只读取 bounded `TextPattern2` caret 前缀；查询失败、密码框或焦点变化时保持 `context_available=false`，不会读取文档范围、聊天列表或预输入内容。Qt 的 `IMR_RECONVERTSTRING` 会改变选区，因此不用于前文读取。

`OnTestKeyDown` 只做轻量探测，不在探测阶段打开 TSF edit session 或请求服务；它先检查 Weasel 同样使用的焦点、`GUID_COMPARTMENT_KEYBOARD_DISABLED`、`GUID_COMPARTMENT_EMPTYCONTEXT` 和只读状态，并拒绝不能创建 composition 的虚空 TSF context。Telegram 的 Qt 控件例外地会在这里预热一个不阻塞的 UIA MTA 查询，但不会读取文本或等待结果。UIA 结果完成后缓存，后续按键读取缓存；如果查询仍在进行，当前按键继续按无上文处理，不阻塞输入路径。部分宿主会在探测回调期间持有 TSF 锁，候选读取和组合更新仍统一在 `OnKeyDown` 中执行。

MPC-BE 的播放画面可能仍有可写的默认 IME context。适配层额外检查当前线程的原生焦点：仅窗口类为 `MPC-BE` 的主窗口，以及该窗口下 `Afx:` 类、控件 ID 为 `AFX_IDW_PANE_FIRST`（`0xe900`）的直接子播放视图，按非编辑区域透传所有按键并清理旧组合状态；不改变中英文模式、不请求候选、不创建组合串。打开文件、搜索、播放列表命名等编辑控件不匹配此规则；不沿祖先或 owner 禁用整个应用。这是基于 [MPC-BE 1.9.0 窗口定义](https://github.com/Aleksoid1978/MPC-BE/blob/1.9.0/include/mpc_defines.h#L23)及其[播放视图创建与焦点路由](https://github.com/Aleksoid1978/MPC-BE/blob/1.9.0/src/apps/mplayerc/MainFrm.cpp#L715)的兼容规则，不是通用输入框检测，也不涵盖独立的独占全屏窗口。未知窗口继续使用原有 TSF 门禁，不能仅凭缺少 Win32 caret 或 `TF_SS_TRANSITORY` 禁用输入。

TSF 的 `RequestEditSession` 结果以 `phrSession` 输出参数为准，并使用 `TF_ES_READWRITE` 的 ASYNCDONTCARE 调度：宿主允许时同步执行，否则由 TSF 排队。`StartComposition` 即使返回 `S_OK` 也可能通过空的 `ppComposition` 表示宿主拒绝组合；适配层会检查这一点并显示明确错误。

新增拼音的写会话被 `TF_E_READONLY` 拒绝时，丢弃本次新增字符，保留此前有效的拼音，并继续消费该按键、显示只读提示；被丢弃的字符不会混入下一次正常输入。焦点门禁已确认 context 不可用，或出现断开、无 selection、宿主拒绝 composition 等不可用 context 错误时，清空 Lime 的本地组合状态并把当前按键交还宿主，避免浏览器失焦后缓存按键在下一个输入框回放；已有有效 composition 遇单独的暂时性 `E_FAIL` 仍保留可重试状态。

中文模式下的独立标点（空格除外）即使没有现有拼音组合串，也会建立并立即结束一个短生命周期 TSF composition；无组合串和候选时的独立空格透传给宿主，让视频播放器等宿主继续响应播放/暂停快捷键。有候选或组合串时，空格的 keydown/keyup 成对由 TSF 消费，用于选择候选或提交组合。不在按键回调中直接调用 `ITfInsertAtSelection` 修改宿主 selection，避免 Chromium/WebView2 等文本上下文的重入崩溃。

候选分页支持 PageUp/PageDown、未移位的主键盘 `-`/`=`、小键盘 `-`/`+` 和 Weasel 滚轮。TSF 只按需要请求候选页面，重排候选数量由服务内部处理；翻页超出已加载范围时只追加 Rime 候选，不重新调用 LLM，也不产生新的历史记录。中文模式下，宿主刚刚收到数字后紧接的句号保留为半角 `.`；退格和其他按键会重置这一状态。F1–F12 等功能键由宿主处理，不参与 Lime 输入。Esc 或退格清空最后一个字母时，会先删除 TSF 组合范围再结束组合；取消完成前继续吞键，避免按键进入宿主文本。

`third_party/weasel-ui` 是 Windows TSF 构建的必需依赖；缺少源码时 CMake 直接失败，不再切换到另一套候选窗口。普通桌面宿主直接读取 Lime 自有目录中的主题；受限宿主通过 Lime 桌面服务取得同一主题文本，避免主题文件访问失败后回退到竖排黑边框默认样式。主题加载顺序和用户目录见 [`docs/design/weasel-ui-integration.md`](../../../docs/design/weasel-ui-integration.md)。
