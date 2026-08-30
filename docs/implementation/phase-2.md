# Phase 2 实现记录

日期：2026-08-30

## 目标

完成 Windows TSF 适配层的生产输入路径：TSF/COM 注册与生命周期、拼音 preedit、前文读取、Rust Named Pipe 请求、原生候选窗口、分页/选择/提交，以及服务不可用时的英文透传。

## 已落地

- `platform/windows/tsf/lime_tsf.cpp` 重组 context probe 的 TSF 注册、COM class factory、`ITfTextInputProcessorEx` 和 `ITfKeyEventSink` 实现。
- 通过 `ITfContextComposition` 管理组合串；字母键更新 preedit，退格/ESC 清理，回车、空格和数字键提交候选或原始拼音。
- 使用 `ITfRangeACP` 优先读取光标前最多 128 个 UTF-16 单元，失败时回退到 anchor shifting；读取结果只在输入请求中传给 Rust 服务。
- 增加 4 字节 little-endian 长度前缀 + UTF-8 JSON Named Pipe 客户端，连接后执行 protocol v1 握手；支持 `LIME_PIPE` 覆盖管道名。
- 首次连接失败时可通过 `LIME_SERVICE_PATH` 启动本地 `lime-service`，随后自动重试；连接/握手/解析失败立即清除候选并让按键透传。
- 增加原生 Win32 popup 候选窗，显示 1-9 编号、当前选中项和分页结果；PageUp/PageDown 翻页，候选窗不依赖 Tauri。
- 连接成功后读取 `get_status` 的配置 revision；输入请求携带最新 revision，服务配置更新导致的过期响应不会覆盖当前候选。
- 保留敏感控件不做特殊处理的首期决策；不记录原始前文、preedit 或候选分数。

## 关键文件

- `platform/windows/tsf/lime_tsf.h`
- `platform/windows/tsf/lime_tsf.cpp`
- `platform/windows/tsf/tsf_module.cpp`
- `platform/windows/tsf/CMakeLists.txt`

## 验证

已在 Windows Visual Studio 18.9.1 / Windows SDK 10.0.26100.0 环境通过：

```powershell
cmake -S platform/windows/tsf -B build/tsf -A x64
cmake --build build/tsf --config Release
```

产物：`build/tsf/Release/lime-tsf.dll`。

由于当前仓库没有签名安装包、librime/Rime 资源或可自动化的真实编辑器宿主，TSF 注册、候选定位、高 DPI 和真实文本控件兼容性仍需在 Phase 4 的 Windows 验收矩阵中执行。
