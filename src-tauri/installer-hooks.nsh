; Progress bridge used by the branded R-Code bootstrapper.
; The normal NSIS UI and updater remain fully functional when /RC_PROGRESS is absent.

!macro RCodeWriteProgress VALUE
  ClearErrors
  ${GetOptions} $CMDLINE "/RC_PROGRESS=" $R8
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
