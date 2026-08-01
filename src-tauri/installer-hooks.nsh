; Progress bridge used by the branded R-Code bootstrapper.
; The normal NSIS UI and updater remain fully functional when /BRANDED_PROGRESS is absent.

!macro RCodeWriteProgress VALUE
  ClearErrors
  ${GetOptions} $CMDLINE "/BRANDED_PROGRESS=" $R8
  ${IfNot} ${Errors}
    FileOpen $R9 "$R8" w
    ${IfNot} ${Errors}
      FileWrite $R9 "${VALUE}"
      FileClose $R9
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro RCodeWriteProgress "installing"
!macroend

!macro NSIS_HOOK_POSTINSTALL
  !insertmacro RCodeWriteProgress "finalizing"
!macroend

; The MCP host lives inside app data and keeps the SQLite database open after the
; desktop window exits. Tauri's first app-data deletion attempt can therefore
; leave the whole profile behind. Only stop hosts and remove a Codex registration
; when every ownership check points at R-Code's managed app-data directory.
!macro RCodeReleaseManagedAppData
  DetailPrint "正在释放 R-Code 本地数据..."
  nsExec::ExecToStack `"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$$ErrorActionPreference='SilentlyContinue'; $$dataRoot=[IO.Path]::GetFullPath((Join-Path ([Environment]::GetFolderPath('ApplicationData')) '${BUNDLEID}\r-code')); $$hostRoot=[IO.Path]::GetFullPath((Join-Path $$dataRoot 'mcp-host')); try { $$codex=Get-Command codex -ErrorAction SilentlyContinue; if ($$codex) { $$registration=(& $$codex.Source mcp get r-code --json 2>$$null | Out-String | ConvertFrom-Json); $$registeredCommand=[IO.Path]::GetFullPath([string]$$registration.transport.command); $$registeredArgs=@($$registration.transport.args); $$ownsRegistration=([string]::Equals([string]$$registration.name,'r-code',[StringComparison]::OrdinalIgnoreCase) -and [string]::Equals([string]$$registration.transport.type,'stdio',[StringComparison]::OrdinalIgnoreCase) -and $$registeredCommand.StartsWith($$hostRoot,[StringComparison]::OrdinalIgnoreCase) -and [IO.Path]::GetFileName($$registeredCommand).StartsWith('r-code-mcp-host-',[StringComparison]::OrdinalIgnoreCase) -and $$registeredArgs.Count -eq 3 -and [string]::Equals([string]$$registeredArgs[0],'mcp-server',[StringComparison]::OrdinalIgnoreCase) -and [string]::Equals([string]$$registeredArgs[1],'--data-dir',[StringComparison]::OrdinalIgnoreCase) -and [string]::Equals([IO.Path]::GetFullPath([string]$$registeredArgs[2]),$$dataRoot,[StringComparison]::OrdinalIgnoreCase)); if ($$ownsRegistration) { & $$codex.Source mcp remove r-code *>$$null } } } catch {}; Get-CimInstance Win32_Process | Where-Object { try { $$exe=[IO.Path]::GetFullPath([string]$$_.ExecutablePath); $$line=[string]$$_.CommandLine; $$exe.StartsWith($$hostRoot,[StringComparison]::OrdinalIgnoreCase) -and [IO.Path]::GetFileName($$exe).StartsWith('r-code-mcp-host-',[StringComparison]::OrdinalIgnoreCase) -and $$line -match '(^|\s)mcp-server(\s|$$)' -and $$line -match '(^|\s)--data-dir(\s|$$)' } catch { $$false } } | ForEach-Object { Stop-Process -Id $$_.ProcessId -Force -ErrorAction SilentlyContinue }; Start-Sleep -Milliseconds 750"`
  Pop $R7
  Pop $R6
  ${If} $R7 != 0
    DetailPrint "R-Code MCP 清理命令返回：$R7"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    !insertmacro RCodeReleaseManagedAppData

    ; Retry after the managed MCP host has released r-code.db. /REBOOTOK is the
    ; final safety net for antivirus/indexer locks that the uninstaller cannot own.
    SetShellVarContext current
    RMDir /r /REBOOTOK "$APPDATA\${BUNDLEID}"
    RMDir /r /REBOOTOK "$LOCALAPPDATA\${BUNDLEID}"

    ${If} ${FileExists} "$APPDATA\${BUNDLEID}"
    ${OrIf} ${FileExists} "$LOCALAPPDATA\${BUNDLEID}"
      MessageBox MB_OK|MB_ICONEXCLAMATION "部分 R-Code 本地数据仍被 Windows 占用，删除将在 Windows 重启后完成。"
    ${EndIf}
  ${EndIf}
!macroend
