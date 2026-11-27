; Prism uninstaller hooks (bundle > windows > nsis > installerHooks in tauri.conf.json).
;
; Prism changes a few things in Windows: an AI block in the hosts file, the "Prism"
; logon task and the WinDivert driver service. Before 0.13 they stayed behind after
; uninstall. `prism.exe --cleanup` reverts them (see src/cleanup.rs), and with the
; "Delete the application data" checkbox `--purge` also removes %ProgramData%\Prism.
;
; Nothing is removed in update mode (/UPDATE): the new version keeps working as before.
; The hook runs before the uninstaller checks for a running app, so --cleanup closes
; a running Prism itself: its watchdog would otherwise restart winws.exe.

!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode <> 1
    ${If} $DeleteAppDataCheckboxState = 1
      nsExec::Exec '"$INSTDIR\${MAINBINARYNAME}.exe" --cleanup --purge'
    ${Else}
      nsExec::Exec '"$INSTDIR\${MAINBINARYNAME}.exe" --cleanup'
    ${EndIf}
    Pop $R9
  ${EndIf}
!macroend
