# 代码签名与自动更新

本文档说明 R-Code 的代码签名和自动更新（updater）配置。

## 1. 代码签名

### 1.1 Windows

#### 开发测试（self-signed）

```powershell
# 生成 self-signed 证书（仅本机有效，SmartScreen 仍会警告）
powershell -ExecutionPolicy Bypass -File scripts/setup-codesign.ps1
```

脚本会生成：
- `codesign-dev.pfx`（私钥，已 .gitignore）
- `codesign-dev.cer`（公钥，需手动安装到"受信任的根证书颁发机构"）

然后在 `src-tauri/tauri.conf.json` 配置：

```json
"bundle": {
  "windows": {
    "certificatePath": "codesign-dev.pfx",
    "certificatePassword": "rcode-dev"
  }
}
```

#### 生产签名（EV/OV 证书）

1. 购买 Windows 代码签名证书（EV ~$300/年，OV ~$200/年）
2. EV 证书通常以 USB Token 形式提供，OV 证书可导出 .pfx
3. 配置 `tauri.conf.json`：

```json
"bundle": {
  "windows": {
    "certificatePath": "path/to/prod-cert.pfx",
    "certificatePassword": "$env:CODESIGN_PASSWORD"
  }
}
```

或在 CI 中用 secrets：

```yaml
env:
  TAURI_SIGNING_CERTIFICATE_DATA: ${{ secrets.WINDOWS_CERT_PFX }}
  TAURI_SIGNING_CERTIFICATE_PASSWORD: ${{ secrets.WINDOWS_CERT_PASSWORD }}
```

| 证书类型 | SmartScreen | 适合 |
| --- | --- | --- |
| self-signed | ❌ 警告 | 开发测试 |
| OV | ⚠️ 需积累信誉 | 小规模发布 |
| EV | ✅ 立即通过 | 正式发布 |

### 1.2 macOS

1. 加入 Apple Developer Program（$99/年）
2. 创建 "Developer ID Application" 证书
3. 配置 `tauri.conf.json`：

```json
"bundle": {
  "macOS": {
    "signingIdentity": "Developer ID Application: Your Name (XXXXXXXXXX)"
  }
}
```

4. 公证（Notarization）：打包后用 `xcrun notarytool` 提交 Apple 公证

CI 中用 secrets：

```yaml
env:
  APPLE_CERTIFICATE: ${{ secrets.APPLE_CERTIFICATE }}
  APPLE_CERTIFICATE_PASSWORD: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}
  APPLE_ID: ${{ secrets.APPLE_ID }}
  APPLE_PASSWORD: ${{ secrets.APPLE_PASSWORD }}
  APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
```

## 2. 自动更新（Updater）

### 2.1 架构

```
用户应用  --(检查更新)-->  GitHub Releases/latest.json  --(下载+验签)-->  安装更新
```

- **签名密钥对**：`.tauri/r-code.key`（私钥，已 .gitignore）+ `.tauri/r-code.key.pub`（公钥，已配置到 `tauri.conf.json`）
- **更新 manifest**：`latest.json`，由 `cargo tauri build` 自动生成（`createUpdaterArtifacts: true`）
- **发布渠道**：GitHub Releases（`https://github.com/charter/r-code/releases/latest/download/latest.json`）

### 2.2 CI Secrets 配置

在 GitHub 仓库 Settings -> Secrets and variables -> Actions 添加：

| Secret 名 | 值 | 说明 |
| --- | --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | `.tauri/r-code.key` 文件内容 | 更新包签名私钥 |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | （空） | 私钥密码（本密钥无密码） |

### 2.3 发布流程

```powershell
# 1. 本地打 tag
git tag v0.1.1
git push origin v0.1.1

# 2. GitHub Actions 自动触发 release.yml：
#    - Windows: 构建 .exe/.msi + latest.json（已签名）
#    - macOS: 构建 .app/.dmg + latest.json（已签名）
#    - 上传 artifacts

# 3. 手动创建 GitHub Release（或用 actions/create-release 自动化）：
#    - 上传 latest.json 到 Release assets
#    - 上传各平台安装包
#    - 用户应用通过 endpoints URL 检查并下载更新
```

### 2.4 前端调用 Updater API

在 `src-tauri/frontend/app.js` 中：

```javascript
// 检查更新
const { check } = window.__TAURI__.updater;
const update = await check();
if (update?.available) {
  const ok = confirm(`发现新版本 ${update.version}，是否更新？\n\n${update.body}`);
  if (ok) {
    await update.downloadAndInstall();
    await relaunch();
  }
}
```

> 注意：前端调用 updater 需要 `tauri.conf.json` 的 `app.security` 允许 updater API，或在 capabilities 中声明。Tauri 2 默认 `dynamic-acl` 特性已启用。

### 2.5 密钥管理

- **私钥** `.tauri/r-code.key`：绝对不能提交（已 .gitignore），丢失则无法发布更新
- **公钥** `.tauri/r-code.key.pub`：已内嵌到 `tauri.conf.json` 的 `plugins.updater.pubkey`
- **轮换密钥**：如需更换，运行 `cargo tauri signer generate -w .tauri/r-code.key -f`，同步更新 `tauri.conf.json` 的 pubkey 和 CI secrets

## 3. 当前配置状态

| 项目 | 状态 | 说明 |
| --- | --- | --- |
| Updater 插件 | ✅ 已集成 | `tauri-plugin-updater` 依赖 + main.rs 注册 |
| 签名密钥对 | ✅ 已生成 | `.tauri/r-code.key`（私钥）+ pubkey 已配置 |
| `createUpdaterArtifacts` | ✅ true | `cargo tauri build` 自动生成 `latest.json` |
| 更新 endpoint | ✅ GitHub Releases | `releases/latest/download/latest.json` |
| CI 签名 | ⚠️ 需配 secrets | `TAURI_SIGNING_PRIVATE_KEY` 待加到 GitHub |
| Windows 代码签名 | ⚠️ 未配置 | 需运行 `setup-codesign.ps1` 或购买证书 |
| macOS 代码签名 | ⚠️ 未配置 | 需 Apple Developer 证书 |
