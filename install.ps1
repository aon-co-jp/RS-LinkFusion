# RS-LinkFusion インストールスクリプト(Windows / Windows Server 共通)。
#
# 使い方(管理者権限のPowerShellで):
#   Invoke-WebRequest -Uri "https://github.com/aon-co-jp/RS-LinkFusion/releases/latest/download/rs-linkfusion-windows-x86_64.zip" -OutFile rs-linkfusion.zip
#   Expand-Archive rs-linkfusion.zip -DestinationPath rs-linkfusion
#   cd rs-linkfusion
#   .\install.ps1

#Requires -RunAsAdministrator

$ErrorActionPreference = "Stop"

$InstallDir = "C:\Program Files\RS-LinkFusion"
$ServiceName = "RSLinkFusion"

Write-Host "==> インストール先: $InstallDir"
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

$BinSrc = Join-Path $PSScriptRoot "rs-linkfusion.exe"
if (-not (Test-Path $BinSrc)) {
    Write-Error "rs-linkfusion.exe が見つかりません($BinSrc)。zipを展開したディレクトリで実行してください。"
    exit 1
}
Copy-Item $BinSrc -Destination $InstallDir -Force

$existing = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
if ($existing) {
    Write-Host "==> 既存のWindowsサービスが見つかったため、バイナリのみ更新しました(再起動は行いません)"
    Write-Host "    手動で再起動する場合: Restart-Service $ServiceName"
} else {
    Write-Host "==> まず鍵を生成してください:"
    Write-Host "      & '$InstallDir\rs-linkfusion.exe' generate-key"
    Write-Host "==> Windowsサービスとして登録する場合の手順(serve側の例、connectの場合はサブコマンドを読み替え):"
    Write-Host "      New-Service -Name $ServiceName -BinaryPathName '$InstallDir\rs-linkfusion.exe serve --bind 0.0.0.0:5900 --target 127.0.0.1:8080 --key <上記の鍵>' -DisplayName 'RS-LinkFusion' -StartupType Automatic"
    Write-Host "      Start-Service $ServiceName"
}

Write-Host "==> 完了。"
