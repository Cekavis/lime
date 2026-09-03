# WeaselUI 视觉层集成方案

## 结论

当前阶段不应直接把完整 Weasel（`WeaselTSF`、`WeaselServer`、Rime IPC）替换进 Lime。Lime 已经有自己的 TSF 状态机、Named Pipe 协议和 Rust 核心；完整 fork 会把这些边界重新耦合。

建议采用“固定版本的 WeaselUI 视觉库 + Lime 自己的 TSF/IPC”方案：只集成 `WeaselUI` 静态库及其必要的公共头文件，输入事件、Rime session、分页和提交仍由 Lime 管理。

上游检查（2026-08-31）：[`rime/weasel`](https://github.com/rime/weasel) `master` 指向 `d73f6295e8252ed2f7b9c12bae32e9001b1afdaa`（固定提交链接：[commit](https://github.com/rime/weasel/commit/d73f6295e8252ed2f7b9c12bae32e9001b1afdaa)）。Weasel 仓库声明 GPLv3；`WeaselUI` 是独立 static target，源码包含 `Layout`、`StandardLayout`、`HorizontalLayout`、`VerticalLayout`、`DirectWriteResources` 和 `WeaselPanel` 等模块。其公开 `UI` 接口是 `Create/Destroy/Show/Hide/Refresh/UpdateInputPosition/Update`，数据模型是 `Context + Status + UIStyle`。当前 Lime 将这一固定版本导出到 `third_party/weasel-ui`，并在 CMake 中默认启用；`LIME_WITH_WEASEL_UI=OFF` 可保留内置回退渲染器。

## 许可证与分发

- Weasel 主仓库为 GPLv3。若 Lime 的 Windows TSF DLL 静态链接修改后的 WeaselUI，应将该组件及其衍生部分按 GPLv3 分发，并提供对应源码、修改说明和许可证文本。
- Rust 核心、Tauri 管理窗口可以保持独立许可证，但发布包中必须明确 Windows TSF/WeaselUI 的许可证边界；最终许可证组合应由发布前法律审查确认。
- 不能把上游源码只放在构建机或私有 submodule 中而不随源码发布。

## submodule、vendored snapshot 与 GitHub fork

推荐顺序：

1. 在 GitHub 建立 Lime 自己的 `weasel-ui` fork（只保留必要的 WeaselUI 修改），以 tag 或不可变 commit 作为上游同步锚点。
2. Lime 主仓库使用固定 commit 的 submodule（例如 `third_party/weasel-ui`），CI 必须执行 `git submodule update --init --recursive`，并在源码归档/发布包中包含 submodule 内容或可获取的对应源码。
3. 如果 Windows 构建环境经常不带 submodule，改用 vendored snapshot（git subtree 或定期导出的目录）更稳；保留 `UPSTREAM_COMMIT`、GPLv3 和修改补丁。

不建议跟踪 `master`、使用未锁定分支，或把完整 Weasel 仓库作为 submodule。完整仓库包含 Rime、部署器、安装器等与 Lime 无关的目标，会放大构建时间和许可证审计面。当前工作区采用固定 commit 的 vendored snapshot，原因是它能保证离线构建、源码归档完整，而且不要求构建者具备 GitHub 权限；后续仓库治理稳定后，可把同一目录迁移为 Lime 组织下的 fork + 固定 commit submodule。

## 构建依赖与 CMake 适配

Weasel 的官方构建要求 Visual Studio C++（ATL/MFC）、CMake、Boost，并通过递归 submodule 获取依赖；官方 `WeaselUI/xmake.lua` 本身只生成 static target、编译全部 `*.cpp` 并启用 `/openmp`。Lime 的适配目标只编译 WeaselUI 源码，不引入 Weasel 的 IPC 序列化，因此 Boost serialization 头文件在 Lime 构建中被条件化；OpenMP 默认关闭以避免额外的运行时 DLL 依赖，可通过 `LIME_WEASEL_UI_OPENMP=ON` 显式打开。Lime CMake 目标补充：

- WeaselUI 源码和 `include/`、WTL 头文件路径；
- `d2d1`、`dwrite`、`gdiplus`、`windowscodecs` 等图形库；
- Unicode、C++20 和与 TSF DLL 相同的 x64 toolset；`/openmp` 仅在显式启用时加入。

不要把 WeaselServer/WeaselTSF 的 IPC 和 Rime 库同时链接到 Lime TSF；Lime 只需要把自己的候选快照适配成 WeaselUI `Context`。

## Lime 到 WeaselUI 的最小适配

新增一个 `WeaselUiAdapter`（建议放在 `platform/windows/tsf/weasel_ui_adapter.{h,cpp}`），职责仅包括：

1. 把 Lime 候选快照映射为 `weasel::Context::cinfo.candies/comments/labels`，设置 `currentPage/totalPages/highlighted/is_last_page`。
2. 不把 `preedit_` 映射为 `Context::preedit`：未确认拼音属于 TSF composition，由宿主文本控件渲染；adapter 只把服务状态映射为 `Status::composing/disabled/ascii_mode`。
3. 把 TSF 获取到的 caret RECT 传给 `UI::UpdateInputPosition`；不要让 WeaselUI 读取 TSF context。
4. 每次状态机产生新快照时调用 `UI::Update`; 失焦、取消或提交后调用 `UI::Hide`，销毁时调用 `UI::Destroy(true)`。
5. WeaselUI 的鼠标/滚轮回调只报告“候选索引/翻页意图”；adapter 将其转换成现有数字键、PageUp/PageDown 或上下键事件，让 TSF 线程继续拥有 context、提交和 IPC，UI 层不直接提交文本或发 IPC。

## preceding_text 显示

Weasel 的 `Context` 没有 preceding-text 字段。Lime 不修改 `WeaselIPCData::Context` 的序列化布局，而是在视觉层增加 sidecar：

- `include/WeaselUI.h` 的 `UI` 增加 `SetPrecedingText(std::wstring_view)` 和只读 accessor；
- `UI::Update` 在调用方没有提供 `Context::aux` 时，把 sidecar 合并为 Weasel 的 auxiliary row；显式 Rime auxiliary 内容优先；
- 这样复用了 WeaselPanel 已有的字体、布局、DPI 和省略逻辑，不需要另画一套候选窗口；前文作为 auxiliary row 显示，拼音不在候选窗重复出现；键盘翻页和滚轮仍由 Lime 状态机处理；
- 不修改 `WeaselIPCData::Context` 的序列化布局，因此不会破坏原有 Weasel IPC。

Lime 在每次候选快照更新时调用 setter；TSF 已按配置读取并裁剪前文，默认预览窗口为 32 个 UTF-16 单元。空字符串不占 auxiliary 行高度。该改动只影响视觉层，不改变按键、分页、选择或提交状态机。

## 主题迁移

`resources/rime/weasel.yaml` 已包含 `style`、`layout`、`preset_color_schemes`。当前 adapter 在 UI 线程启动时读取 Weasel 兼容 YAML 的实用子集并生成 `weasel::UIStyle`，不把 yaml-cpp 引入 TSF。文件按以下顺序加载，后者覆盖前者：内置资源、安装包 `rime/weasel.yaml`、DLL 旁 `weasel.yaml`、`%APPDATA%\\Rime\\weasel.yaml`、`%APPDATA%\\Rime\\weasel.custom.yaml`、`LIME_WEASEL_YAML` 指定文件。这样用户可直接迁移 Weasel 的常用 `style/layout/preset_color_schemes` 自定义。

WeaselUI 期望的是已经解析好的 `weasel::UIStyle`，而不是 YAML 文件路径。当前实现先在 TSF 进程内解析，未来若需要完整 YAML 语义可改由 Rust core 返回扁平 `ui_style` 对象；C++ adapter 只做字段和颜色格式转换。后者可以：

- 兼容现有 Weasel 自定义主题字段；
- 避免在 TSF DLL 中再引入 yaml-cpp；
- 让配置 revision 与候选快照一起更新，避免半套主题；
- 对未知字段采用 Weasel 默认值并记录一次诊断信息。

首期已映射 `font_face`、`label_font_face`、`comment_font_face`、字号、`inline_preedit`、横/竖排、布局间距、圆角/边框/阴影和主要候选颜色。为避免重复拼音行，adapter 固定使用空的 `Context::preedit` 和 inline-preedit 能力；`weasel.yaml` 中的应用级设置（`app_options`）仍由 TSF/核心策略处理，不交给 WeaselUI；无法解析的字段保留 Weasel 兼容默认值。

## 阻塞点与验证

- 当前 snapshot 已包含 WeaselUI、WTL 头文件、GPLv3 文本和上游 commit 记录；Boost serialization 仅在上游 IPC 代码需要时启用。
- 已在 Windows x64 的 MSVC/Windows SDK 环境通过 WeaselUI ON 和内置回退 OFF 两种 CMake 构建；尚未在真实记事本、浏览器和高 DPI 多显示器上完成手工验收。
- 鼠标候选选择和滚轮已通过 adapter 转发到 TSF 键盘状态机；在 inline-preedit 模式下 Weasel 内置页箭头不绘制，键盘分页与滚轮仍可用。WeaselPanel 增加了 mouse-up 的直接行命中，避免依赖跨线程 hover 状态。鼠标悬停本身不改变 Lime 的选中项，避免 UI 线程直接修改 TSF 状态。
- 发布前必须继续验证：无候选隐藏、失焦自动隐藏、空格/回车提交、鼠标/滚轮不抢焦点、不同完整性级别进程下的 `SendInput` 行为、每监视器 DPI、主题覆盖，以及 GPL 对应源码归档。
