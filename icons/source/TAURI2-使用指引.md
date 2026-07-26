# R Code 图标：Tauri 2 使用指引

本指引用于将本资源包中的图标配置到 Tauri 2 桌面应用中，目标平台为 Windows 和 macOS。

## 1. 选择要使用的文件

本资源包中已包含可直接使用的文件：

| Tauri 目标 | 资源包内文件 | 复制到项目后的文件名 |
| --- | --- | --- |
| Windows 安装包与应用 | `windows/r-code-app.ico` | `src-tauri/icons/icon.ico` |
| macOS 应用与 DMG | `macos/r-code-app.icns` | `src-tauri/icons/icon.icns` |
| PNG 图标（32 px） | `windows/png/r-code-32.png` | `src-tauri/icons/32x32.png` |
| PNG 图标（128 px） | `windows/png/r-code-128.png` | `src-tauri/icons/128x128.png` |
| PNG 图标（256 px） | `windows/png/r-code-256.png` | `src-tauri/icons/128x128@2x.png` |
| PNG 图标（512 px） | `windows/png/r-code-512.png` | `src-tauri/icons/icon.png` |

如果项目还没有 `src-tauri/icons/` 文件夹，请先创建它。

> 不要直接把原始图片或 `master/` 目录中的母版放进 Tauri 配置。Tauri 打包应使用上述已按平台导出的 ICO、ICNS 和 PNG 文件。

## 2. 配置 `tauri.conf.json`

打开项目中的 `src-tauri/tauri.conf.json`，确认包含或合并以下配置：

```json
{
  "productName": "R Code",
  "identifier": "com.yourcompany.rcode",
  "bundle": {
    "active": true,
    "icon": [
      "icons/32x32.png",
      "icons/128x128.png",
      "icons/128x128@2x.png",
      "icons/icon.png",
      "icons/icon.icns",
      "icons/icon.ico"
    ]
  }
}
```

将 `com.yourcompany.rcode` 改成你自己的唯一标识，例如：

```json
"identifier": "com.rcode.desktop"
```

发布后不要随意修改 `identifier`；它是 macOS 的 Bundle ID，也会影响 Windows 安装包身份。

## 3. 目录示例

完成后，项目结构应类似：

```text
your-project/
└─ src-tauri/
   ├─ icons/
   │  ├─ 32x32.png
   │  ├─ 128x128.png
   │  ├─ 128x128@2x.png
   │  ├─ icon.png
   │  ├─ icon.ico
   │  └─ icon.icns
   └─ tauri.conf.json
```

## 4. 打包

### Windows

在 Windows 电脑上、项目根目录执行：

```powershell
cargo tauri build --bundles nsis,msi
```

通常会生成：

- `target/release/bundle/nsis/`：`.exe` 安装程序
- `target/release/bundle/msi/`：`.msi` 安装程序

如只需一种安装包，可只打其中一种：

```powershell
cargo tauri build --bundles nsis
```

### macOS

必须在 Mac 上构建：

```bash
cargo tauri build --bundles app,dmg
```

通常会生成：

- `target/release/bundle/macos/`：`.app`
- `target/release/bundle/dmg/`：`.dmg`

## 5. 构建后检查

发布前请至少确认：

- Windows：开始菜单、任务栏、桌面快捷方式和安装程序均显示 R Code 图标。
- macOS：Finder 中的 `.app` 和 DMG 中显示 R Code 图标。
- 不应再出现原图外围的黑色方形画布。
- 将窗口缩小或查看任务栏时，图标仍然可辨识。

如果更改图标后仍显示旧图标，先卸载旧版本，再清理构建产物后重新打包；Windows 和 macOS 都可能缓存应用图标。

## 6. 正式发布必做项

- Windows：为安装包和应用程序做代码签名，降低 SmartScreen 警告概率。
- macOS：用 Apple Developer 证书签名并完成 notarization（公证），否则用户打开时可能被 Gatekeeper 拦截。
- 始终在实际 Windows 和实际 macOS 设备上分别安装测试最终安装包。

## 可选：macOS 新版 Icon Composer

若你后续使用 Xcode 的 Icon Composer，可使用：

- `macos/IconComposerLayers/background-1024.png`
- `macos/IconComposerLayers/foreground-1024.png`

这不是 Tauri 常规打包的必要步骤；对于 Tauri 2，直接使用 `icon.icns` 已足够。
