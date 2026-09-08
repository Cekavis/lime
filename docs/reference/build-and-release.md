# 构建与发布

## 输出目录

所有生成内容写入 `out/`：

```text
  out/
  cargo/                         Rust/Cargo target
  cargo-tauri/                   standalone Tauri Cargo target (local checks)
  windows-x64/
    downloads/                   已校验的下载缓存
    sources/                     固定 commit 的第三方源码
    work/                        第三方压缩包解压与组装临时目录
    builds/                      llama.cpp CMake 构建目录
    patched/                     通过 patch 的 llama.cpp DLL
    staging/app/                 Tauri 资源 staging
    cmake/tsf/                   TSF 构建目录
  cargo/release/bundle/nsis/     NSIS 安装包和 SHA-256 文件
```

`target/` 和 `build/` 不再作为发布脚本的输入或输出；手工构建也使用 `out/` 下的对应目录。

## 第三方资源

`third_party/rime/librime-1.17.0.json` 固定 librime 与雾凇版本、下载地址和 SHA-256；`third_party/llama/b10743/` 固定 llama.cpp 源码 commit、runtime 包和 patch。`third_party/rime/` 还保留 Lime 使用的许可证和默认 Weasel 主题，`third_party/weasel-ui/` 与 Rust wrapper 也位于同一目录。

准备源码：

```powershell
powershell -ExecutionPolicy Bypass -File tools/prepare-third-party-sources.ps1
```

单独准备 Rime runtime：

```powershell
powershell -ExecutionPolicy Bypass -File tools/prepare-rime-runtime.ps1
```

发布脚本默认从固定源码 checkout 开始，自动应用 patch，构建 patched `llama.dll`，再把它覆盖到对应的 CPU/CUDA 官方 backend runtime 中。patched DLL 必须带 `.lime-output-reorder-patched` marker；backend DLL 仍来自匹配的官方 runtime 包。

## 发布流程

```powershell
powershell -ExecutionPolicy Bypass -File tools/build-windows.ps1
```

流程固定为：

1. 构建 `lime-service` 和 Windows TSF。
2. 下载并校验固定的 librime、雾凇和 llama.cpp 资源。
3. 校验 llama.cpp 源码 commit，应用 patch 并生成 patched DLL。
4. 将 patched DLL 与匹配的 CPU/CUDA backend DLL 组装到 staging。
5. 将服务、TSF、Rime 和许可证复制到 staging。
6. 构建前端和 NSIS 安装包，并生成 SHA-256 文件。

发布入口始终从固定源码 checkout 开始编译 patched `llama.dll`，再把同一份经过 provenance 校验的 DLL 叠加到 CPU/CUDA runtime；这样不会因为本机环境变量或旧产物误选 runtime。
