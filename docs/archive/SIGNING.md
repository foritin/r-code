# 代码签名与自动更新

本文档说明 R-Code 的代码签名和自动更新（updater）配置。

## 1. 代码签名

### 1.1 Windows

#### 开发测试（self-signed）

```powershell
# 生成 self-signed 证书（仅本机有效，SmartScreen 仍会警告）
powershell -ExecutionPolicy Bypass -File scripts/setup-dev-codesign.ps1
```

脚本会交互式读取 PFX 密码，不会把密码写入源码或终端输出。

脚本会生成：
- `codesign-dev.pfx`（私钥，已 .gitignore）
- `codesign-dev.cer`（公钥，需手动安装到"受信任的根证书颁发机构"）

证书同时创建在当前用户的个人证书库中。使用脚本输出的证书指纹做本机临时配置：

```json
"bundle": {
  "windows": {
    "certificateThumbprint": "<脚本输出的证书指纹>",
    "digestAlgorithm": "sha256"
  }
}
```

如需让本机信任测试签名，再将 `codesign-dev.cer` 导入本机的“受信任的根证书颁发机构”。上述配置仅供本地临时测试，不要提交机器相关的证书指纹、PFX 文件或密码。

#### 生产可信签名

公开发布有三条常见路径：

- **Microsoft Store + MSIX**：Store 会重新签名，通常不需要自行购买证书。
- **符合条件的开源项目**：可申请 [SignPath Foundation](https://signpath.org/) 免费托管签名。
- **直接分发 MSI/NSIS**：向公共 CA 申请 OV 或 EV 代码签名证书，或使用 CA 提供的远程签名服务。微软列出的常见颁发机构包括 DigiCert、GlobalSign、Sectigo 和 SSL.com。

申请传统 CA 证书通常需要提交真实的个人或组织身份、地址和联系电话，并完成邮件/电话或组织登记验证。自 2023-06-01 起，公开信任代码签名证书的私钥必须保存在合规硬件中，因此新 OV 和 EV 证书通常使用 USB Token、HSM 或托管签名服务，不应预期得到可自由导出的 PFX。

微软 Artifact Signing（原 Trusted Signing）适合 CI/CD，但其 Public Trust 有地区限制；截至 2026-07，中国大陆不在支持范围。中国大陆发布者更现实的选择通常是 Microsoft Store、SignPath Foundation（开源且符合条件时），或可向中国大陆主体签发并交付硬件 Token/远程签名的公共 CA。

拿到证书后，根据供应商的交付方式配置 Tauri：本机 Token/证书库可使用 `bundle.windows.certificateThumbprint`；HSM 或远程服务使用 `bundle.windows.signCommand`。正式签名应使用 SHA-256 和 RFC 3161 时间戳，并用 `signtool verify /pa /v <安装包>` 验证。

| 证书类型 | SmartScreen | 适合 |
| --- | --- | --- |
| self-signed | 与未签名相同，公开下载仍会警告 | 开发测试 |
| OV / EV | 显示已验证发布者，但新应用仍需积累信誉 | 官网直接发布 |
| Microsoft Store + MSIX | Store 安装不会触发下载 SmartScreen 警告 | 面向普通用户发布 |

> EV 已不再自动绕过 SmartScreen；不要仅为“首发无警告”支付 EV 溢价。参考微软的 [Windows 代码签名选项](https://learn.microsoft.com/windows/apps/package-and-deploy/code-signing-options) 和 [SmartScreen 信誉说明](https://learn.microsoft.com/windows/apps/package-and-deploy/smartscreen-reputation)。

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
- **发布渠道**：GitHub Releases（`https://github.com/foritin/r-code/releases/latest/download/latest.json`）

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
| Windows 代码签名 | ⚠️ 未配置 | 本地测试可运行 `setup-dev-codesign.ps1`；正式发布需可信证书或签名服务 |
| macOS 代码签名 | ⚠️ 未配置 | 需 Apple Developer 证书 |
