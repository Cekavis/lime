# Windows TSF 适配层（Phase 0）

这里是 Windows 10 22H2+ x64 的 C++ DLL 工程骨架。当前只包含 DLL/COM 生命周期占位：`DllMain` 禁止在线程入口执行 COM 初始化，`LimeTsfInitialize`/`LimeTsfShutdown` 在调用线程执行最小 `CoInitializeEx`/`CoUninitialize`，注册导出返回 `E_NOTIMPL`。

Phase 2 将在此处接入 TSF text service、preedit、前文读取、原生候选窗口与 Rust Named Pipe 客户端。实时输入路径不应依赖 Tauri 管理窗口。

## 构建

需要：Visual Studio 2022 的 “Desktop development with C++” 工作负载（MSVC x64/x86 工具集、Windows 10/11 SDK）以及 CMake 3.20+。MSBuild/SDK 本身不足以直接配置本目录的 CMake 工程。

在 Windows Developer PowerShell 中：

```powershell
cmake -S platform/windows/tsf -B build/tsf -A x64
cmake --build build/tsf --config Release
```

非 Windows 主机上配置会明确失败；本目录不携带 SDK、librime 或任何预编译二进制。
