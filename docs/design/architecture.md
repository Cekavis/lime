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
          │ current-user Named Pipe
          ▼
Lime Core Rust service
  ├─ IPC server & request generations
  ├─ configuration owner
  ├─ RimeEngine (librime + 雾凇)
  ├─ LlmReranker (llama.cpp + GGUF)
  ├─ user dictionary
  └─ privacy-safe structured logs
          ▲
          │ same IPC API
Tauri 2 management window
  └─ settings / model / dictionary / diagnostics
```

macOS 未来只替换最上层平台适配器和候选 UI，复用 Rust 服务 API。

## 实时输入流程

1. TSF 接收按键并更新 preedit。
2. TSF 读取光标前文本，按设置裁剪字符窗口；读取失败按空上下文处理。
3. TSF 请求 Rust 服务生成候选。
4. Rust 调用 Rime 获取候选并过滤不能完整覆盖拼音的短候选。
5. Rime 候选先返回/显示；若启用 LLM，异步对候选池重排。
6. Rust 返回新的候选顺序；TSF 仅在 request generation 仍然有效时替换显示。
7. 用户选择候选后由 TSF 提交 `commit_text`，Rust 可通知 Rime 学习。

## 服务生命周期

- TSF 首次需要中文候选时按需启动 Rust 服务；服务按当前用户单实例常驻。
- Tauri 打开时连接已有服务，不能假设主窗口先启动。
- 模型切换执行受控重载；重载期间状态为 `reloading`，中文暂不可用并透传英文。
- 模型缺失/关闭/加载失败：服务保持 `rime_only`。
- Rust 服务不可用：TSF 进入英文、数字、常用标点透传；恢复后重新握手。

## 平台范围

- 首期交付 Windows 10 22H2+ x64。
- Windows TSF 适配层为 C++，复用现有 context probe 的验证结论。
- Rust 核心保持跨平台；macOS 适配器只定义接口，不在首期实现。
