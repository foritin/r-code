# R-Code 应用图标

本目录存放 Tauri 2 打包直接使用的应用图标，以及未来重制图标所需的源素材。
`src-tauri/tauri.conf.json` 的 `bundle.icon` 通过 `../icons/...` 引用这些文件。

## 打包文件

| 文件 | 用途 |
| --- | --- |
| `32x32.png` | Linux / 通用小尺寸 |
| `128x128.png` | Linux / 通用中尺寸 |
| `128x128@2x.png` | 高 DPI 256×256 |
| `512x512.png` | Linux hicolor 512 档 |
| `icon.png` | 1024×1024 通用源图 |
| `icon.ico` | Windows 应用与安装包 |
| `icon.icns` | macOS 应用与 DMG |

这些文件属于发布输入，不应移动到 `docs/` 或删除。

## 源素材

`source/` 只保留可维护的母版和图层，不再保存重复的发布 ZIP：

- `source/master/r-code-icon-master-1024-fullbleed.png`：Windows / Linux 满铺母版。
- `source/master/r-code-icon-master-1024.png`：保留透明外沿的通用母版。
- `source/master/r-code-icon-light-1024.png`：亮色备用母版。
- `source/layers/background-fullbleed-1024.png`：满铺背景图层。
- `source/layers/background-rounded-plate-1024.png`：透明画布圆角底板。
- `source/layers/foreground-mark-1024.png`：独立前景标记。

Windows / Linux 图标使用满铺母版，避免系统不会自动套遮罩时主体显得过小。macOS 的 `icon.icns` 保留系统图标网格所需的内缩留白，不应直接由满铺 PNG 替换。

## 替换与验证

替换图标时应同步更新上表中的 7 个打包文件并保持文件名不变，然后分别验证：

- Windows：安装程序、开始菜单、任务栏和桌面快捷方式。
- macOS：`.app` 与 DMG。
- Linux：AppImage / DEB 安装后的应用菜单与窗口图标。

系统可能缓存旧图标；必要时卸载旧版本并清理 `target/` 后重新打包。
