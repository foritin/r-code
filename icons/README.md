# R-Code 应用图标

本目录存放 Tauri 2 打包使用的应用图标，被 `src-tauri/tauri.conf.json` 的 `bundle.icon` 字段引用。

## 文件清单

| 文件 | 用途 | 来源（资源包） |
| --- | --- | --- |
| `32x32.png` | Linux / 通用小尺寸 | `windows/png/r-code-32.png` |
| `128x128.png` | Linux / 通用中尺寸 | `windows/png/r-code-128.png` |
| `128x128@2x.png` | 高 DPI（256×256） | `windows/png/r-code-256.png` |
| `icon.ico` | Windows 安装包与可执行文件 | `windows/r-code-app.ico` |
| `icon.icns` | macOS 应用与 DMG | `macos/r-code-app.icns` |

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
