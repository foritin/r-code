# 生成 self-signed 代码签名证书（仅用于开发测试，不能过 SmartScreen）
# 生产发布请购买 EV/OV 证书。
#
# 用法：在项目根目录运行 `powershell -ExecutionPolicy Bypass -File scripts/setup-codesign.ps1`

$ErrorActionPreference = "Stop"

Write-Host "=== 生成 R-Code 开发用 self-signed 代码签名证书 ===" -ForegroundColor Cyan

# 1. 创建自签名代码签名证书
$cert = New-SelfSignedCertificate `
    -Type CodeSigningCert `
    -Subject "CN=R-Code Dev, O=R-Code Team, C=CN" `
    -KeyAlgorithm RSA -KeyLength 4096 `
    -HashAlgorithm SHA256 `
    -KeyUsage DigitalSignature `
    -KeyExportPolicy Exportable `
    -CertStoreLocation "Cert:\CurrentUser\My" `
    -NotAfter (Get-Date).AddYears(3)

# 2. 导出 .pfx（含私钥，供 tauri-build 签名用）
$pfxPath = Join-Path $PSScriptRoot "..\codesign-dev.pfx"
$pwd = ConvertTo-SecureString -String "rcode-dev" -Force -AsPlainText
Export-PfxCertificate -Cert $cert -FilePath $pfxPath -Password $pwd

# 3. 导出 .cer（公钥，需安装到"受信任的根证书颁发机构"才能本机信任）
$cerPath = Join-Path $PSScriptRoot "..\codesign-dev.cer"
Export-Certificate -Cert $cert -FilePath $cerPath

Write-Host ""
Write-Host "证书已生成：" -ForegroundColor Green
Write-Host "  PFX (私钥): $pfxPath"
Write-Host "  CER (公钥): $cerPath"
Write-Host ""
Write-Host "下一步：" -ForegroundColor Yellow
Write-Host "  1. 双击 .cer 文件 -> 安装证书 -> 本地计算机 -> 受信任的根证书颁发机构"
Write-Host "     （这样本机才会信任该 self-signed 签名）"
Write-Host "  2. 在 tauri.conf.json 的 bundle.windows 配置："
Write-Host '     "certificatePath": "codesign-dev.pfx",'
Write-Host '     "certificatePassword": "rcode-dev"'
Write-Host ""
Write-Host "  3. 确保 .gitignore 包含 codesign-dev.pfx（私钥不能提交）"
Write-Host ""
Write-Host "⚠️  注意：self-signed 证书仅用于开发测试，SmartScreen 仍会警告。" -ForegroundColor Red
Write-Host "   生产发布请购买 EV 证书（立即通过 SmartScreen）或 OV 证书（需积累信誉）。"
