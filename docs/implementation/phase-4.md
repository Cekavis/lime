# Phase 4 实现记录

日期：2026-08-30

## 目标

把 Phase 0–3 的 Windows 组件组装成可安装、可卸载、可升级的当前用户安装包，并提供验收与发布门禁。安装包不携带模型、Rime 用户词库或其他用户数据。

## 已落地

- `src-tauri/tauri.conf.json` 开启 Tauri NSIS bundle，仅支持 Windows x64 发布；资源包含 release `lime-service.exe`、TSF `lime-tsf.dll` 和雾凇拼音 `rime/` 目录。
- `src-tauri/windows/nsis/hooks.nsh`：安装后注册 TSF COM/profile，并写入当前用户的 `LIME_SERVICE_PATH`；卸载前注销 TSF，并仅在值仍指向本次安装目录时删除该环境变量。由于 Windows TSF profile/category 注册需要系统级注册表写权限，NSIS 安装模式为 `perMachine`，安装时会请求 UAC 管理员许可。
- 管理窗口和 TSF 在环境变量尚未被新进程继承时，会从自身安装目录的 `lime-service.exe` 自动发现服务（同时兼容 `resources/lime-service.exe`）；服务未显式设置 `LIME_DATA_DIR` 时使用 `%LOCALAPPDATA%\\Lime`，确保首次安装、升级和 TSF 独立启动都共享同一份用户数据。
- `tools/release/build-windows.ps1`：按固定顺序构建 Rust 服务、Windows TSF、前端和 NSIS 安装包，并生成旁车 SHA-256 校验文件；在 GitHub Actions 中自动从 `vMAJOR.MINOR.PATCH` tag 注入安装包版本。
- `.github/workflows/release.yml`：`vMAJOR.MINOR.PATCH` tag 在 Windows x64 上执行同一构建脚本，上传安装包及校验文件并创建 GitHub Release 草稿。
- `docs/implementation/phase-4-acceptance.md`：记录安装、升级、卸载、TSF、Rime-only、LLM 和服务崩溃透传验收步骤。

## 用户数据与升级语义

安装器使用 NSIS `perMachine` 模式并在安装时请求 UAC 管理员许可。核心服务默认将数据写入当前登录用户的 `%LOCALAPPDATA%\\Lime`；安装目录同时包含固定版本的雾凇拼音 schema/词库资源，服务在无模型时自动加载核心词库并进入 Rime-only。安装/升级只替换安装目录中的程序文件，不删除用户配置、词库或模型。卸载会移除程序、TSF 注册和本次安装写入的服务路径，不主动删除用户数据。

## 本地构建

在 Windows Developer PowerShell 中执行：

```powershell
powershell -ExecutionPolicy Bypass -File tools/release/build-windows.ps1
```

产物位于 `src-tauri/target/release/bundle/nsis/`，同目录生成 `.sha256` 校验文件。当前仓库不提交这些二进制产物。

## 验证边界

自动化构建可验证 Rust/前端/TSF 编译、资源打包和校验文件生成；候选窗口已改为独立 Win32 UI 线程（含 DPI、圆角和状态提示），但真实 TSF 注册、候选窗口定位、不同编辑器兼容性和安装/升级/卸载仍需在 Windows 10 22H2+ x64 主机上按验收矩阵手工执行。
