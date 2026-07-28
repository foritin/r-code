# R-Code 工作台交互 Demo

直接打开 `index.html` 即可体验。Demo 默认使用亮色主题，左下角可切换亮色/暗色；主题偏好保存在浏览器本机。

## 交互范围

- 工作台启动器：运行与子代理、审核、终端、文件
- 工作台切换、隐藏、重新打开与专注模式
- 子代理选择、公开进度、停止确认
- 终端命令模拟、隐藏后恢复、新建与结束确认
- 文件筛选、文件选择、工具切换后恢复
- 审核文件切换、请求修改、回滚、接受与收起后恢复
- 任务之间的工作台和文件状态隔离
- 亮色/暗色、宽屏停靠与窄屏覆盖布局

## 截图参数

`index.html?state=review&theme=dark`

- `state`: `launcher`、`run`、`terminal`、`files`、`review`、`review-collapsed`
- `theme`: `light`、`dark`

## 验证

仓库本身不引入 Playwright 依赖。使用 Codex 工作区附带的 Node 运行时执行：

```powershell
$env:NODE_PATH='C:\Users\huang\.cache\codex-runtimes\codex-primary-runtime\dependencies\node\node_modules'
& 'C:\Users\huang\.cache\codex-runtimes\codex-primary-runtime\dependencies\node\bin\node.exe' .\qa.cjs
```

默认截图写入 `target/ui-demo/`。只有需要重建正式原型图时才添加 `--update-prototypes`。
