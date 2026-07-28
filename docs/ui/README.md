# R-Code UI — Current

这是当前唯一有效的 UI 评审入口。静态原型与交互 Demo 现在由同一份实现生成，不再维护两套视觉标准。

## 最新原型（1600 × 1000）

| 状态 | 亮色 | 暗色 |
| --- | --- | --- |
| 工作台启动器 | [查看](./prototypes/workbench/01-launcher-light.png) | [查看](./prototypes/workbench/02-launcher-dark.png) |
| 运行与子代理详情 | [查看](./prototypes/workbench/03-subagents-light.png) | [查看](./prototypes/workbench/04-subagents-dark.png) |
| 集成终端 | [查看](./prototypes/workbench/05-terminal-light.png) | [查看](./prototypes/workbench/06-terminal-dark.png) |
| 文件 Peek 与项目树 | [查看](./prototypes/workbench/07-files-light.png) | [查看](./prototypes/workbench/08-files-dark.png) |
| 审核、Diff 与变更树 | [查看](./prototypes/workbench/09-review-light.png) | [查看](./prototypes/workbench/10-review-dark.png) |
| 审核摘要收起 | [查看](./prototypes/workbench/11-review-collapsed-light.png) | [查看](./prototypes/workbench/12-review-collapsed-dark.png) |

原型图说明见 [prototypes/workbench/README.md](./prototypes/workbench/README.md)。

- `state=launcher|run|terminal|files|review|review-collapsed`
- `theme=dark|light`

## 设计结论

- 右侧采用可停靠宽工作台，不沿用 340–414px 的全局 inspector。
- 主对话始终稳定；终端、文件、审核和子代理在一个排他的工作台槽位中切换。
- 子代理从运行列表下钻，不显示内部事件 JSON 或私有推理。
- 待处理审核收起后仍是同一审核摘要，点击恢复完整审核；不会替换成项目动态。
- 工具画布使用连续平面和分隔线，不做卡片套卡片。

完整行为和视觉合同见 [SPEC.md](./SPEC.md)，能力边界见 [BACKEND-CONTRACT.md](./BACKEND-CONTRACT.md)。Codex 参照事实记录在 [PRODUCT-FACTS.md](./PRODUCT-FACTS.md)。

## 当前 Demo（唯一实现）

[打开交互 Demo](./demo/index.html)

支持启动器、工具切换、隐藏与恢复、专注模式、子代理详情与停止确认、终端状态保持、文件选择、审核收起/展开与决策确认，以及任务状态隔离。默认亮色，主题入口位于左下角。

确定性截图参数：

- `state=launcher|run|terminal|files|review|review-collapsed`
- `theme=light|dark`

## 重新渲染原型

使用可解析 Playwright 的 Node 环境：

```powershell
node demo/qa.cjs
```

默认输出到 `target/ui-demo/`。脚本会验证 6 个状态 × 2 个主题 × 4 个视口、浏览器错误、页面溢出、按钮裁切和关键交互。只有明确传入 `--update-prototypes` 时，才会覆盖 `prototypes/workbench/` 下 12 张正式原型图。
