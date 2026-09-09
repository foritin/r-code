; Progress bridge used by the branded R-Code bootstrapper.
; The normal NSIS UI and updater remain fully functional when /BRANDED_PROGRESS is absent.

; 用户 PATH 写入后广播 WM_SETTINGCHANGE：新开终端无需注销/重启即可看到变更。
!include "WinMessages.nsh"

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
      ; 前缀校验可被 `..\` 相对段绕过（如 $TEMP\..\..\Web\...）。归一层追加
      ; 拒绝：`..` 作为完整路径段出现时必然命中 `\..\` / `/../`（中段），
      ; 或以 `\..` / `/..` 结尾（末段），任一命中即拒绝写入。
      StrCpy $R5 "0"
      ${StrLoc} $R6 $R8 "\..\" ">"
      ${If} $R6 != ""
        StrCpy $R5 "1"
      ${EndIf}
      ${StrLoc} $R6 $R8 "/../" ">"
      ${If} $R6 != ""
        StrCpy $R5 "1"
      ${EndIf}
      ${If} $R5 == "0"
        StrCpy $R6 $R8 "" -3
        ${If} $R6 == "\.."
        ${OrIf} $R6 == "/.."
          StrCpy $R5 "1"
        ${EndIf}
      ${EndIf}
      ${If} $R5 != "1"
        FileOpen $R9 "$R8" w
        ${IfNot} ${Errors}
          FileWrite $R9 "${VALUE}"
          FileClose $R9
        ${EndIf}
      ${EndIf}
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro RCodeWriteProgress "installing"
!macroend

!macro NSIS_HOOK_POSTINSTALL
  !insertmacro RCodeWriteProgress "finalizing"

  ; M8-04（R-DST-02）：把安装目录写入当前用户 PATH（免提权），使 r-code-tui
  ; CLI 随安装包就位。纯内置指令（ReadRegStr/WriteRegExpandStr，无插件依赖——
  ; Tauri NSIS 模板未内置 Environ 插件）。去重：按 ';' 拆分后逐项做大小写不
  ; 敏感的全等比较，已含 $INSTDIR 则跳过（升级不重复追加）。非致命：失败只
  ; DetailPrint，绝不 Abort。
  ClearErrors
  ReadRegStr $R0 HKCU "Environment" "PATH"
  ${If} $R0 == ""
    WriteRegExpandStr HKCU "Environment" "PATH" "$INSTDIR"
  ${Else}
    ; StrLoc 的首次子串匹配会把仅包含/扩展该路径的条目（如
    ; C:\Program Files\R-Code-CLI\bin）误判为已存在，从而漏加真正的 $INSTDIR。
    StrCpy $R2 "$R0"
    StrCpy $R3 ""
    ${Do}
      ${StrLoc} $R4 $R2 ";" ">"
      ${If} $R4 == ""
        StrCpy $R1 $R2
        StrCpy $R2 ""
      ${Else}
        StrCpy $R1 $R2 $R4
        IntOp $R4 $R4 + 1
        StrCpy $R2 $R2 "" $R4
      ${EndIf}
      ${If} $R1 == $INSTDIR
        StrCpy $R3 "1"
      ${EndIf}
    ${LoopUntil} $R2 == ""
    ${If} $R3 != "1"
      WriteRegExpandStr HKCU "Environment" "PATH" "$R0;$INSTDIR"
    ${EndIf}
  ${EndIf}
  ${If} ${Errors}
    DetailPrint "PATH 接入跳过（用户 PATH 写入失败，主程序安装不受影响）"
  ${Else}
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
    DetailPrint "已把 R-Code 安装目录加入用户 PATH（重启终端生效）"
  ${EndIf}
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
  ; M8-04：从用户 PATH 移除安装目录（卸载清理；纯内置指令，非致命不阻断卸载）。
  ; 按 ';' 拆分后只移除与 $INSTDIR 完全相等（忽略大小写）的条目：旧的 StrLoc
  ; 首次子串拼接式截断会把仅包含该路径前缀的无关条目（如
  ; C:\Program Files\R-Code-CLI\bin）一并损坏。$INSTDIR 恰为整条 PATH 时重建
  ; 结果为空串（合法，与旧行为一致）。
  ClearErrors
  ReadRegStr $R0 HKCU "Environment" "PATH"
  ${If} $R0 != ""
    StrCpy $R2 "$R0"
    StrCpy $R3 ""
    StrCpy $R6 ""
    ${Do}
      ${StrLoc} $R4 $R2 ";" ">"
      ${If} $R4 == ""
        StrCpy $R1 $R2
        StrCpy $R2 ""
      ${Else}
        StrCpy $R1 $R2 $R4
        IntOp $R4 $R4 + 1
        StrCpy $R2 $R2 "" $R4
      ${EndIf}
      ${If} $R1 == $INSTDIR
        StrCpy $R6 "1"
      ${Else}
        ${If} $R3 == ""
          StrCpy $R3 "$R1"
        ${Else}
          StrCpy $R3 "$R3;$R1"
        ${EndIf}
      ${EndIf}
    ${LoopUntil} $R2 == ""
    ${If} $R6 == "1"
      WriteRegExpandStr HKCU "Environment" "PATH" "$R3"
      ${IfNot} ${Errors}
        SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
      ${EndIf}
    ${EndIf}
  ${EndIf}
  ${If} ${Errors}
    DetailPrint "用户 PATH 清理跳过（不影响卸载继续）"
  ${EndIf}

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
