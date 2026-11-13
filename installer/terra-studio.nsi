; Terra Studio installer. Build: makensis -DSRC=<folder with exe, dll and extras> terra-studio.nsi
Unicode true
!include "MUI2.nsh"

!define APP "Terra Studio"
!define VERSION "2.4.1"
!define UNKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\TerraStudio"
!ifndef SRC
  !define SRC "..\..\pkg\TerraStudio"
!endif

Name "${APP}"
OutFile "TerraStudio-Setup-${VERSION}.exe"
; Per-user install, so no admin prompt.
RequestExecutionLevel user
InstallDir "$LOCALAPPDATA\Programs\TerraStudio"
InstallDirRegKey HKCU "${UNKEY}" "InstallLocation"
SetCompressor /SOLID lzma

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "FileDescription" "${APP} Setup"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "Sigit"

!define MUI_ICON "..\assets\icon.ico"
!define MUI_UNICON "..\assets\icon.ico"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\TerraStudio.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Open Terra Studio now"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Terra Studio"
  ; A running copy locks the exe; data in %APPDATA%\TerraStudio is not touched.
  nsExec::Exec 'taskkill /IM TerraStudio.exe /F'
  Sleep 800
  SetOutPath "$INSTDIR"
  File "${SRC}\TerraStudio.exe"
  File "${SRC}\WebView2Loader.dll"
  File "${SRC}\READ-ME-FIRST.txt"
  Delete "$INSTDIR\BACA-DULU.txt"
  ; First start imports these two files from the exe folder. -DNOCHANNEL leaves them out (GitHub build).
  !ifndef NOCHANNEL
    File "${SRC}\profil-terra.json"
    File "${SRC}\Content_*.zip"
  !endif
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\TerraStudio.exe" "" "$INSTDIR\TerraStudio.exe" 0
  CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\TerraStudio.exe" "" "$INSTDIR\TerraStudio.exe" 0

  WriteRegStr HKCU "${UNKEY}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${UNKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNKEY}" "Publisher" "Sigit"
  WriteRegStr HKCU "${UNKEY}" "DisplayIcon" "$INSTDIR\TerraStudio.exe"
  WriteRegStr HKCU "${UNKEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNKEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "${UNKEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNKEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNKEY}" "NoRepair" 1
  WriteRegDWORD HKCU "${UNKEY}" "EstimatedSize" 2700
SectionEnd

Section "Uninstall"
  nsExec::Exec 'taskkill /IM TerraStudio.exe /F'
  Sleep 800
  Delete "$INSTDIR\TerraStudio.exe"
  Delete "$INSTDIR\WebView2Loader.dll"
  Delete "$INSTDIR\BACA-DULU.txt"
  Delete "$INSTDIR\READ-ME-FIRST.txt"
  Delete "$INSTDIR\profil-terra.json"
  Delete "$INSTDIR\Content_*.zip"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$DESKTOP\${APP}.lnk"
  DeleteRegKey HKCU "${UNKEY}"
SectionEnd
