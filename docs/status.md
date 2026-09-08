# 项目状态

日期：2026-09-08

Lime 当前以 Windows 10 22H2+ x64 为首要交付目标。实时输入链路是同步请求：TSF 发送输入快照，Rust 服务获取 Rime 候选并在已加载模型时完成 LLM 排序后返回最终候选。服务不可用时 TSF 进入英文、数字和标点透传；没有模型时保持 Rime-only。

已完成：

- Rust workspace 已拆出 `lime-ipc` 和 `lime-service`。
- 管理窗口只依赖 `lime-ipc` 与 `lime-protocol`，不再依赖 `lime-core`。
- 机器可读契约集中在 `contracts/`。
- 构建与发布产物统一写入 `out/`。
- 第三方资源统一由 `third_party/` manifest 管理；源码、二进制、patch 和构建结果分层保存。
- 发布脚本校验固定源码 commit、自动应用 llama.cpp patch，并使用 patched `llama.dll` 组装 CPU/CUDA runtime。

发布前仍需验证：

- Windows TSF 注册、编辑器兼容性、候选定位和输入透传。
- Rime/雾凇 runtime 在干净环境中的加载。
- CUDA 与 CPU patched llama.cpp 的真实 GGUF logits smoke test。
- NSIS 安装、升级、卸载和用户数据保留。

前端保持现有 vanilla DOM 实现。组件化 UI、生成 TypeScript DTO 和更细的 TSF 文件拆分记录为后续改进方向。
