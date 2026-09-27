# 架构设计

## 目标

Lime 将实时输入路径与管理 UI 解耦：Windows TSF 负责接收按键和展示候选，Rust 核心负责所有跨平台输入业务，Tauri 负责配置管理。

## 进程与模块

```text
Windows TSF C++ DLL / host
  ├─ TSF/COM lifecycle
  ├─ preedit & key state
  ├─ preceding-text reader
  ├─ native candidate UI
  └─ IPC client
          │ compatibility-first Named Pipe
          ▼
lime-service Rust process
  ├─ lime-ipc framing and platform listener
  ├─ lime-core request handling and generations
  ├─ configuration owner
  ├─ RimeEngine (librime + 雾凇)
  ├─ LlmReranker (llama.cpp + GGUF)
  ├─ user dictionary
  └─ privacy-safe structured logs
          ▲
          │ same protocol through lime-ipc
Tauri 2 management window
  └─ settings (models + memory) / test / benchmark / dictionary / history
```

代码布局保持这些边界：`crates/lime-management/` 是标准 Tauri 应用根目录，`src/` 放 Vite 管理页面，`src-tauri/` 放桌面壳和 IPC 命令；`lime-core` 按 engine、llama、ranking、service 子模块组织运行时；Windows TSF 的 `lime_tsf.cpp` 通过同一翻译单元包含职责独立的 `.inl` 文件，以保留 COM 私有状态和内部链接，同时避免继续维护单个超大源文件。

macOS 未来只替换最上层平台适配器和候选 UI，复用 Rust 服务 API。

## 实时输入流程

1. TSF 接收按键并更新 preedit。
2. TSF 读取光标前文本，按设置裁剪字符窗口；读取失败按空上下文处理。
3. TSF 通过 `lime-ipc` 请求 Rust 服务生成候选，并等待本次请求完成。
4. Rust 只调用 librime 获取候选，不自行解析词库、补造候选或按猜测过滤结果。
5. 如果存在上文和可用模型，Rust 在同一请求内完成候选筛选与 LLM 排序；上文为空或模型不可用时直接采用 Rime 顺序。服务可为排序读取超出当前页面的候选，但只向 TSF 返回 `candidates` 与 `candidate_remainders` 的请求页面前缀。候选窗口翻页超出已加载范围时使用 Rime-only 扩展请求，不重新调用 LLM，并保持已返回的最终排序前缀。
6. Rust 返回最终候选顺序；服务在返回前检查 request generation，过期请求被丢弃，不会覆盖较新的输入。
7. 用户选择候选后由 TSF 提交 `commit_text`；响应同时携带候选未覆盖的原始拼音，部分选择后由 TSF 保留剩余组合串，Rust 可通知 Rime 学习。

## 服务生命周期

- TSF 首次需要中文候选时按需启动 Rust 服务；服务按当前用户单实例常驻。
- Tauri 打开时连接已有服务，不能假设主窗口先启动。
- 模型切换执行受控重载；重载期间状态为 `reloading`，中文暂不可用并透传英文。
- 模型缺失/关闭/加载失败：服务保持 `rime_only`。
- Rust 服务不可用：TSF 进入英文、数字、常用标点透传；恢复后重新握手。

Windows Named Pipe 为兼容开始菜单、系统设置等 packaged/AppContainer 宿主，允许普通、
低完整性和 packaged 客户端连接；当前版本不额外实现调用方身份校验或用户隔离。

## 平台范围

- 首期交付 Windows 10 22H2+ x64。
- Windows TSF 适配层为 C++，复用现有 context probe 的验证结论。
- Rust 核心保持跨平台；macOS 适配器只定义接口，不在首期实现。
