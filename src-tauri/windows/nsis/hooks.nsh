; Phase 4 Windows integration hooks.
; The TSF DLL registers its COM class/profile machine-wide.  The profile enable
; bit and service path remain user-scoped so each interactive user can choose
; Lime independently.

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Registering Lime Windows text service"
  ExecWait '"$SYSDIR\regsvr32.exe" /s "$INSTDIR\lime-tsf.dll"' $0
  IntCmp $0 0 +2 0 0
    DetailPrint "Warning: TSF registration returned code $0"

  ; Enable the Lime profile for the installing user. The TSF API also sets the
  ; machine default; this HKCU write covers repair/upgrade and a profile that
  ; Windows previously left disabled. It does not select Lime as the active
  ; keyboard layout; the user still chooses it in the language bar/settings.
  WriteRegDWORD HKCU "Software\Microsoft\CTF\TIP\{2F7A6C4C-2A0B-4EF0-9D7E-D4143D648012}\LanguageProfile\0x00000804\{6F7D47B0-5A33-4D61-9595-900ED395B61A}" "Enable" 1

  WriteRegExpandStr HKCU "Environment" "LIME_SERVICE_PATH" "$INSTDIR\lime-service.exe"
  System::Call 'user32::SendMessageTimeout(i 0xffff, i ${WM_SETTINGCHANGE}, i 0, t "Environment", i 0, i 5000, *i .r0) i .r1'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  DetailPrint "Unregistering Lime Windows text service"
  ExecWait '"$SYSDIR\regsvr32.exe" /u /s "$INSTDIR\lime-tsf.dll"' $0

  ReadRegStr $1 HKCU "Environment" "LIME_SERVICE_PATH"
  StrCmp $1 "$INSTDIR\lime-service.exe" 0 +2
    DeleteRegValue HKCU "Environment" "LIME_SERVICE_PATH"
  System::Call 'user32::SendMessageTimeout(i 0xffff, i ${WM_SETTINGCHANGE}, i 0, t "Environment", i 0, i 5000, *i .r0) i .r1'
!macroend
