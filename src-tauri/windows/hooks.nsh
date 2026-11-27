; Prism uninstaller hooks (bundle > windows > nsis > installerHooks in tauri.conf.json).
;
; Prism changes a few things in Windows: an AI block in the hosts file, the "Prism"
; logon task and the WinDivert driver service. Before 0.13 they stayed behind after
; uninstall. `prism.exe --cleanup` reverts them (see src/cleanup.rs), and with the
; "Delete the application data" checkbox `--purge` also removes %ProgramData%\Prism.
; If something cannot be reverted (an antivirus guards the hosts file, for example),
; Prism lists what is left and offers to retry. `--quiet` skips that dialog when the
; uninstaller runs silently.
;
; Nothing is removed in update mode (/UPDATE): the new version keeps working as before.
; The hook runs before the uninstaller checks for a running app, so --cleanup closes
; a running Prism itself: its watchdog would otherwise restart winws.exe.

!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode <> 1
    StrCpy $R9 "--cleanup"
    ${If} $DeleteAppDataCheckboxState = 1
      StrCpy $R9 "$R9 --purge"
    ${EndIf}
    ${If} ${Silent}
      StrCpy $R9 "$R9 --quiet"
    ${EndIf}
    nsExec::Exec '"$INSTDIR\${MAINBINARYNAME}.exe" $R9'
    Pop $R9
  ${EndIf}
!macroend
