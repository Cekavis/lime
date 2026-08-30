# UI 与 Design Tokens

## UI 分工

### Windows 原生候选窗口

由 TSF C++ 适配层实现，负责 preedit、候选、分页、数字键选择、提交和前文预览。前文预览默认显示最近 32 个字符，可设置修改或关闭。

候选项只显示必要信息：候选文本、平台生成的编号和可选的紧凑前文预览。不显示排序分数、延迟、模型名称或内部状态。

### Tauri 管理窗口

使用 Tauri 2、Tailwind、shadcn，包含：

- 输入与候选设置
- GGUF 导入/选择
- Rime 用户词库导入/导出/清空
- 服务状态和最小诊断信息

管理界面不参与实时按键和候选替换。

## Design tokens

首期建立集中 token 文件，至少覆盖：

- color：background、foreground、muted、accent、destructive、border
- typography：font family、size、weight、line height
- spacing：最小间距阶梯
- radius：基础圆角和控件圆角
- elevation：窗口/弹层阴影
- motion：短过渡时长和 easing

组件只能引用语义 token，不得在页面或组件内写任意颜色、字号、间距、圆角和阴影值。新增组件变体必须先更新 token 与组件规范。

## 视觉原则

- 简洁、低干扰、无多余说明文字。
- 设置项使用清晰标签和原生控件，不重复解释实现细节。
- 状态只显示用户需要采取行动的信息：可用、Rime-only、重载中、服务不可用。
- Figma 可作为视觉确认工具，但最终 token 和组件规范必须落入仓库文档/代码。
