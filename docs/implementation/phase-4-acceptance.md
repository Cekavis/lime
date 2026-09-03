# Phase 4 Windows 验收矩阵

在 Windows 10 22H2 或更高版本、x64、普通用户账户执行。每项记录系统版本、Lime 版本、结果和日志位置。

| 场景 | 操作 | 通过标准 |
| --- | --- | --- |
| 管理员安装 | 运行 NSIS `.exe` 并允许 UAC | 安装完成，开始菜单出现 Lime，TSF 注册不返回 code 5，程序可启动 |
| 管理窗口 | 打开 Lime，等待轮询并切换页面 | 服务状态、模型、词库和历史会实时更新；配置保存后保持当前表单；服务不可用时有明确错误 |
| TSF 注册 | 安装/升级后确认 Lime profile 已启用；必要时以管理员重新运行 `regsvr32 lime-tsf.dll` 并重启 Text Services Framework；在“设置 > 时间和语言 > 语言和区域”选择 Lime，打开记事本 | 当前用户 profile `Enable=1`；可切换 Lime；输入拼音显示独立 UI 线程绘制的圆角候选窗口 |
| 前文读取 | 在已有中文文本后继续输入 | 候选请求使用光标前文；读取失败时仍可按空上下文输入 |
| Rime-only | 不加载模型或关闭 LLM | 安装目录雾凇核心词库自动加载，候选可用，顺序与 Rime 基线一致 |
| LLM 重排 | 加载本地 GGUF 并启用 LLM | 仅重排候选，不生成候选；旧请求结果不会覆盖新输入 |
| 服务崩溃 | 结束 `lime-service.exe`，继续输入英文和标点，再恢复服务 | 服务不可用期间英文/数字/标点透传；恢复后中文重新可用 |
| 编辑器兼容性 | 在 Notepad、QQ、Electron 与 WebView2 文本框中切换 Rime-only，输入字母、数字、退格、空格、回车，并连续输入单字符和双字符全角符号；移动编辑器窗口后重复输入 | 编辑器不崩溃或卡死；候选窗跟随输入光标而非屏幕原点；空格/数字键提交候选，回车提交当前英文原文；无拼音时全角符号通过立即结束的 TSF composition 提交；服务可用时字母进入组合串并显示编号/选中态候选，服务不可用或无候选时明确显示英文透传并正常输入；宿主拒绝 TSF composition 时显示明确诊断 |
| 中英模式 | 在 Notepad 中分别短按左 Shift、右 Shift、Shift+Space，并测试 Shift+字母/符号 | 左 Shift 无修饰短按切换中英；右 Shift 和 Shift+Space 不切换；英文模式字母、数字和半角符号直接输入，中文模式显示候选并使用全角标点 |
| 升级保留数据 | 安装旧版本后修改配置/词库，再运行新版本安装器 | 配置、词库和模型路径仍存在 |
| 卸载 | 从“应用和功能”卸载 | TSF 注册移除，程序文件移除；用户数据目录仍保留 |
| 完整性 | 对 `.exe` 和 `.sha256` 执行 `Get-FileHash -Algorithm SHA256` | 哈希一致 |

自动化门禁：`cargo fmt --all -- --check`、`cargo check --workspace`、`cargo test --workspace`、`cargo check --manifest-path src-tauri/Cargo.toml`、`npm --prefix frontend run build`，以及 `tools/release/build-windows.ps1`。
