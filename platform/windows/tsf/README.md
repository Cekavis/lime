# Windows TSF 适配层（Phase 2）

这是 Windows 10 22H2+ x64 的 C++ TSF text service。输入路径包含 COM/TSF 生命周期、组合串 preedit、光标前文读取、Rust Named Pipe v1 客户端、原生候选 popup、分页/选择/提交，以及连接失败时的英文/数字/标点透传。实时输入路径不依赖 Tauri 管理窗口。

服务管道默认使用 `\\.\pipe\lime-core-v1`，可通过 `LIME_PIPE` 覆盖。若设置 `LIME_SERVICE_PATH`，首次连接失败时 TSF 会按需启动该本地服务并重试。

## 构建

需要：Visual Studio 2022 的 “Desktop development with C++” 工作负载（MSVC x64/x86 工具集、Windows 10/11 SDK）以及 CMake 3.20+。MSBuild/SDK 本身不足以直接配置本目录的 CMake 工程。

在 Windows Developer PowerShell 中：

```powershell
cmake -S platform/windows/tsf -B build/tsf -A x64
cmake --build build/tsf --config Release
```

非 Windows 主机上配置会明确失败；本目录不携带 SDK、librime 或任何预编译二进制。
