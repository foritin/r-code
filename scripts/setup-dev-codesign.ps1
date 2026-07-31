# 生成本地开发安装测试使用的 self-signed 代码签名证书。
# 该证书不能替代正式的 Windows 代码签名证书，也不能绕过 SmartScreen。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File scripts/setup-dev-codesign.ps1

[CmdletBinding()]
param(
    [SecureString]$Password
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ($null -eq $Password) {
    $Password = Read-Host "请输入开发 PFX 密码" -AsSecureString
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$pfxPath = Join-Path $repoRoot "codesign-dev.pfx"
$cerPath = Join-Path $repoRoot "codesign-dev.cer"

Write-Host "=== 生成 R-Code 本地开发代码签名证书 ===" -ForegroundColor Cyan

$certificate = New-SelfSignedCertificate `
    -Type CodeSigningCert `
    -Subject "CN=R-Code Dev, O=R-Code Team, C=CN" `
    -KeyAlgorithm RSA `
    -KeyLength 4096 `
    -HashAlgorithm SHA256 `
    -KeyUsage DigitalSignature `
    -KeyExportPolicy Exportable `
    -CertStoreLocation "Cert:\CurrentUser\My" `
    -NotAfter (Get-Date).AddYears(3)

Export-PfxCertificate -Cert $certificate -FilePath $pfxPath -Password $Password | Out-Null
Export-Certificate -Cert $certificate -FilePath $cerPath | Out-Null

Write-Host "证书已生成：" -ForegroundColor Green
Write-Host "  PFX（私钥，本地开发使用）: $pfxPath"
Write-Host "  CER（公钥，可导入本机信任）: $cerPath"
Write-Host ""
Write-Host "不要提交这两个文件，也不要将该证书用于正式发布。" -ForegroundColor Yellow
