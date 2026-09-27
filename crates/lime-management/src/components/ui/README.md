# 共享组件约定

组件源自 shadcn/ui `new-york-v4` 官方 registry，采用其 MIT 许可；Radix 提供焦点、键盘、对话框和选择控件行为。`components.json` 保留组件生成配置。

- 色彩、字体、间距基数、圆角、阴影、弹窗尺寸在 `src/style.css`；页面只用语义色与 Tailwind 共享尺度。
- Button 使用 default / secondary / outline / ghost / destructive / link；主操作实色，普通操作描边，图标和低优先级操作 ghost。删除确认才使用 destructive。
- Card 的 default 变体用于表单内容；rows 变体无内边距和子项间隙，用于设置列表、模型列表和表格。均不附加标题或说明段落。
- Dialog / AlertDialog 使用 `dialog-frame` 统一宽度、最大高度、滚动与阴影。提示文字仅限错误、空状态和不可撤销操作确认。
- 表单统一 Label、Input、Textarea、NativeSelect、Checkbox、Switch；只显示必要标签，不放副标题、字段描述或实现说明。
- 表格在容器内横向滚动；历史窄窗口隐藏的字段可在详情中访问。图标按钮必须有中文可访问名称。
- 设置合页使用 style.css 的 settings-layout 与 settings-column 宽度 token，空间足够时两列等宽，左侧设置、右侧占用表格与模型选择；否则按设置、表格、模型选择单列排列。settings-field-pair 根据表单容器宽度排列字段。占用明细与候选评分直接展示，不增加折叠或说明段落。
- 焦点环统一 ring-2；弹层颜色使用 overlay；破坏性按钮前景使用 destructive-foreground；本地化关闭按钮。
