# R-Code 应用图标

本目录存放 Tauri 2 打包使用的应用图标，被 `src-tauri/tauri.conf.json` 的 `bundle.icon` 字段引用。

## 文件清单

| 文件 | 用途 | 来源 |
| --- | --- | --- |
| `32x32.png` | Linux / 通用小尺寸 | 由图层重制（见「满铺重制」） |
| `128x128.png` | Linux / 通用中尺寸 | 由图层重制 |
| `128x128@2x.png` | 高 DPI（256×256） | 由图层重制 |
| `512x512.png` | Linux hicolor 512 档 / 高 DPI 缩略图 | 由满铺母版 LANCZOS 降采样 |
| `icon.png` | 1024×1024 最大档（AppImage/hicolor 1024、通用源图） | 满铺母版原样 |
| `icon.ico` | Windows 安装包与可执行文件 | 由图层重制（16/20/24/32/40/48/64/96/128/256 十档 PNG） |
| `icon.icns` | macOS 应用与 DMG | `macos/r-code-app.icns`（未改，见下） |

## 满铺重制

原始资源包里，圆角底板只占画布 84%（四周各留约 8% 透明边），前景标记又只占底板
66%、占整张画布 55%。Windows 与 Linux 不会像 macOS 那样自动套系统遮罩，图标是按原
样绘制的，两层留白叠加后主体 R 在任务栏尺寸下过小。

现在 Windows / Linux 的四个文件改为从 `source/layers/` 重新合成：

- 底板：满铺画布，圆角半径取画布的 20%
- 前景 `foreground-mark-1024.png`：按 bbox 居中缩放到画布的 76%
- 小尺寸先按 4× 超采样再降采样，保证 16–32px 的边缘干净
- 合成后的 1024 母版存为 `source/master/r-code-icon-master-1024-fullbleed.png`

`icon.icns` 保持原样：macOS 会自行套圆角遮罩并按自己的网格排版，需要保留内缩留白，
不适用满铺规则。macOS 侧若要重制，走 `source/layers/background-fullbleed-1024.png`
+ `foreground-mark-1024.png` 的 Icon Composer 图层流程，不要拿这里的满铺 PNG 去转。

## 源素材（`source/`）

原始资源包、母版、图层和设计说明保存在 `source/` 子目录：

- `source/r-code-app-icon-release.zip`：完整原始资源包
- `source/README.md`：设计说明（含 Windows / macOS / 图层语义）
- `source/TAURI2-使用指引.md`：Tauri 2 集成指引
- `source/master/r-code-icon-master-1024.png`：1024 母版
- `source/layers/`：背景板与前景图层（便于重制）

## 替换图标

替换图标时，请同步更新根目录下的 5 个打包文件，并保持文件名不变（`tauri.conf.json` 通过文件名引用）。源素材保留在 `source/` 供未来重制。

## 打包

```powershell
# Windows
cargo tauri build --bundles nsis,msi

# macOS（须在 Mac 上执行）
cargo tauri build --bundles app,dmg
```

更换图标后若系统仍显示旧图标，先卸载旧版本并清理 `target/` 后重新打包（Windows 与 macOS 都会缓存应用图标）。
