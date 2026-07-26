# R Code App Icon 发布资源

这套资源由原始 Logo 清理并重新导出，外围黑色画布已经去除。

## Windows

- `windows/r-code-app.ico`：适用于 Electron、Tauri、Win32、安装包和可执行文件。
- `windows/png/`：包含 16–512 px 的独立透明 PNG，可用于 MSIX/WinUI/WPF 等资源槽位。

Electron Builder 示例：将 `windows/r-code-app.ico` 配置为 Windows 的 `icon`。

## macOS

- `macos/r-code-app.icns`：适用于 Electron、Tauri及传统 macOS 打包流程。
- `macos/RCode.iconset/`：完整的传统 iconset，可在 macOS 上重新执行 `iconutil -c icns RCode.iconset`。
- `macos/AppIcon-1024-fullbleed.png`：1024×1024 未预裁系统圆角的扁平母版，适合放入 Xcode AppIcon。
- `macos/IconComposerLayers/`：Apple Icon Composer 使用的背景和前景 PNG 图层。

## 母版与图层

- `master/r-code-icon-master-1024.png`：带透明外沿、保留圆角底板的通用母版。
- `layers/background-fullbleed-1024.png`：macOS 系统遮罩用的满铺背景。
- `layers/background-rounded-plate-1024.png`：透明画布上的圆角背景板。
- `layers/foreground-mark-1024.png`：独立的橙黄色 R、窗口框和代码符号。

## 注意

- macOS 新版系统会自行应用圆角遮罩；不要再次手工给 `AppIcon-1024-fullbleed.png` 裁圆角。
- Windows 小图标已经包含多尺寸资源。若应用在 16 px 下仍需更强识别度，可再制作一个只保留 `R` 的极简专用版本。
- AI 重制文件适合作为发布资源；若未来需要无限放大印刷或严格品牌规范，建议以这些图层为参考补绘矢量 SVG 源稿。
