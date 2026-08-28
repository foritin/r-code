; Progress bridge used by the branded R-Code bootstrapper.
; The normal NSIS UI and updater remain fully functional when /BRANDED_PROGRESS is absent.

!macro RCodeWriteProgress VALUE
  ClearErrors
  ${GetOptions} $CMDLINE "/BRANDED_PROGRESS=" $R8
  ${IfNot} ${Errors}
    ; 只接受 $TEMP 前缀的路径：命令行可携带任意路径，无限制时本宏会以固定
    ; 内容截断该文件（F-sec-08）。$TEMP 由安装器解析，不受命令行控制。
    StrCpy $R7 "$TEMP\"
    StrLen $R6 "$TEMP\"
    StrCpy $R5 $R8 $R6
    ${If} $R5 == $R7
      FileOpen $R9 "$R8" w
      ${IfNot} ${Errors}
        FileWrite $R9 "${VALUE}"
        FileClose $R9
      ${EndIf}
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
!define RCODE_UNINSTALL_CLEANUP_SCRIPT "${__FILEDIR__}\uninstall-cleanup.ps1"

!macro RCodeReleaseManagedAppData
  DetailPrint "正在释放 R-Code 本地数据..."
  InitPluginsDir
  File /oname=$PLUGINSDIR\r-code-uninstall-cleanup.ps1 "${RCODE_UNINSTALL_CLEANUP_SCRIPT}"
  nsExec::ExecToStack `"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\r-code-uninstall-cleanup.ps1" -BundleId "${BUNDLEID}"`
  Pop $R7
  Pop $R6
  Delete "$PLUGINSDIR\r-code-uninstall-cleanup.ps1"
  ${If} $R7 != 0
    DetailPrint "R-Code 本地数据清理返回：$R7 ($R6)"
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
      MessageBox MB_OK|MB_ICONEXCLAMATION "已强制关闭 R-Code 受管进程，但部分本地数据仍被 Windows 或安全软件占用，删除将在 Windows 重启后完成。"
    ${EndIf}
  ${EndIf}
!macroend
