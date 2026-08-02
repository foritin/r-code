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

- `source/master/r-code-icon-bright-1024-fullbleed.png`：当前 Windows / Linux 亮色发布母版。
- `source/master/r-code-icon-bright-1024.png`：当前 macOS 内缩亮色发布母版。
- `source/master/r-code-icon-master-1024-fullbleed.png`：Windows / Linux 满铺母版。
- `source/master/r-code-icon-master-1024.png`：保留透明外沿的通用母版。
- `source/master/r-code-icon-light-1024.png`：亮色备用母版。
- `source/layers/background-fullbleed-1024.png`：满铺背景图层。
- `source/layers/background-rounded-plate-1024.png`：透明画布圆角底板。
- `source/layers/foreground-mark-1024.png`：独立前景标记。

当前发布图标使用亮橙底与近黑标记。Windows `16 / 20 / 24` 像素帧只保留像素对齐的粗体 `R`，`32` 像素帧增加简化代码括号；从 `40` 像素起恢复完整的窗口、圆点和括号构图。这样可避免把复杂 1024 像素母版直接压缩到任务栏尺寸时产生的拥挤与模糊。Windows / Linux 大尺寸资源继续使用满铺母版；macOS 的 `icon.icns` 使用内缩母版，以保留系统图标网格所需的留白。

修改母版后，在仓库根目录执行以下命令同步全部发布文件：

```powershell
.\scripts\generate-app-icons.ps1
```

脚本会先通过 Tauri 生成完整大图，再调用 `scripts/render-small-app-icons.mjs` 覆盖像素对齐的小尺寸帧，最后同步应用、Windows 安装/卸载程序、品牌安装器和 macOS 图标。Windows `icon.ico` 固定包含 16、20、24、32、40、48、64、96、128、256 像素帧，以覆盖常见显示缩放档位。

只调整 Windows / Linux 小图、不需要重写 macOS `icns` 时，可使用 `-WindowsOnly`：

```powershell
.\scripts\generate-app-icons.ps1 -WindowsOnly
```

## 替换与验证

替换图标时应通过生成脚本同步更新上表中的 7 个打包文件并保持文件名不变，然后分别验证：

- Windows：安装程序、开始菜单、任务栏和桌面快捷方式。
- macOS：`.app` 与 DMG。
- Linux：AppImage / DEB 安装后的应用菜单与窗口图标。

系统可能缓存旧图标；必要时卸载旧版本并清理 `target/` 后重新打包。
