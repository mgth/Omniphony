; Omniphony for Windows: the player (mpv-omniphony), Studio with its renderer,
; and the engine library, from one release. Design: docs/installer.md.
;
;   makensis -DTAG=v0.7.0 -DSTAGE=<dir> -DOUTFILE=<setup.exe> omniphony.nsi
;
; STAGE holds player/ (the player zip's files) and studio/ (the Studio
; archive's tree), plus install-files.nsh and uninstall-files.nsh, the two file
; lists build.sh writes from them.
;
; Per user by default, no elevation: files under %LOCALAPPDATA%\Programs\Omniphony,
; the engine in %LOCALAPPDATA%\omniphony\lib where the player, Studio and
; orender look before their own copy. No config file is written.
;
; Switches (after /S for a silent run):
;   /STARTUP          start the renderer when the user logs on (the unchecked
;                     component of the wizard)
;   /ALLUSERS         per machine, under %ProgramFiles%\Omniphony. The wizard
;                     does not offer it; started with it, it elevates through
;                     UAC. A silent run must already be elevated (exit code 2
;                     otherwise).
;   /SERVICE          with /ALLUSERS: install the renderer as a Windows
;   /SERVICE=auto     service, started on demand, or at boot with =auto.
;                     Not offered by the wizard: audio output from session 0
;                     is untested (docs/installer.md, step 6).
;   /D=<dir>          the install directory (last on the line, no quotes)

Unicode true
!include MUI2.nsh
!include LogicLib.nsh
!include FileFunc.nsh
!include Sections.nsh
!include WordFunc.nsh

!ifndef TAG
  !error "pass -DTAG=<release tag>"
!endif
!ifndef STAGE
  !error "pass -DSTAGE=<staged tree>"
!endif
!ifndef OUTFILE
  !error "pass -DOUTFILE=<installer path>"
!endif

!define PRODUCT "Omniphony"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Omniphony"
!define SERVICE_NAME "omniphony-renderer"
; The command line Studio gives the renderer it starts and the service it
; installs (orender_render_args in the Studio core), with Studio's defaults.
!define RENDER_ARGS "render \\.\pipe\orender.input --continuous --enable-vbap --osc --osc-host 127.0.0.1 --osc-port 9000 --osc-rx-port 9000 --osc-yield"
!define STARTUP_LINK "Omniphony Renderer.lnk"

Name "${PRODUCT} ${TAG}"
OutFile "${OUTFILE}"
RequestExecutionLevel user
SetCompressor /SOLID lzma
ManifestDPIAware true
ShowInstDetails show
ShowUninstDetails show

Var Machine        ; "1" per machine (/ALLUSERS), "" per user
Var Service        ; "", "demand" or "auto" (/SERVICE, per machine only)
Var EngineDir      ; the per-user engine folder of the installing account
Var ConfigDir      ; %ProgramData%\omniphony
Var BridgeReport   ; what the last page says about the decoder bridge
Var WasRunning     ; "1" when the service was stopped for an upgrade

; ── Pages ────────────────────────────────────────────────────────────────────
!define MUI_ABORTWARNING
!define MUI_COMPONENTSPAGE_NODESC
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${STAGE}/studio/LICENSE"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_TITLE "Omniphony ${TAG} is installed"
!define MUI_FINISHPAGE_TEXT "$BridgeReport"
!define MUI_FINISHPAGE_LINK "Install page: from nothing to a film playing"
!define MUI_FINISHPAGE_LINK_LOCATION "https://github.com/mgth/Omniphony/blob/main/docs/install/windows.md"
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

; ── Shared by the installer and the uninstaller ──────────────────────────────

; Ends a silent run with a message on the log and a non-zero exit code; a
; wizard shows the message.
!macro Fail MESSAGE
  DetailPrint "${MESSAGE}"
  IfSilent +2
    MessageBox MB_OK|MB_ICONSTOP "${MESSAGE}"
  SetErrorLevel 2
  Abort "${MESSAGE}"
!macroend

; Per machine needs an administrator. A wizard asks once, through UAC, and
; waits for the elevated copy; a silent run is refused instead, since it could
; neither answer the prompt nor hand back that copy's exit code. TARGET is the
; program to start again: the uninstaller runs from a temporary copy, which
; cannot be started a second time.
!macro ElevateIfMachine TARGET KEEP_DIR
  ${If} $Machine == "1"
    UserInfo::GetAccountType
    Pop $0
    ${If} $0 != "Admin"
      ${If} ${Silent}
        SetErrorLevel 2
        Quit
      ${EndIf}
      ${GetParameters} $1
      ; NSIS has already taken /D= off the command line.
      ${If} "${KEEP_DIR}" == "1"
      ${AndIf} $INSTDIR != ""
        StrCpy $1 "$1 /D=$INSTDIR"
      ${EndIf}
      ExecShellWait "runas" "${TARGET}" "$1"
      Quit
    ${EndIf}
  ${EndIf}
!macroend

!macro Folders
  ; A 32-bit installer of 64-bit programs: the registry's 64-bit view, where
  ; %ProgramFiles% installs are looked for.
  SetRegView 64
  SetShellVarContext current
  StrCpy $EngineDir "$LOCALAPPDATA\omniphony\lib"
  ReadEnvStr $ConfigDir ProgramData
  StrCpy $ConfigDir "$ConfigDir\omniphony"
  ${If} $Machine == "1"
    SetShellVarContext all
  ${EndIf}
!macroend

; The renderer service: "" when there is none, "stopped", or "active" while it
; runs, starts or stops. A stopping service still holds orender.exe. Asked of
; the service manager itself rather than read from the text sc.exe prints.
!macro ServiceState RESULT
  StrCpy ${RESULT} ""
  ; SC_MANAGER_CONNECT, then SERVICE_QUERY_STATUS: both open to a plain user.
  System::Call 'advapi32::OpenSCManagerW(p 0, p 0, i 0x0001) p .R0'
  ${If} $R0 P<> 0
    System::Call 'advapi32::OpenServiceW(p R0, w "${SERVICE_NAME}", i 0x0004) p .R1'
    ${If} $R1 P<> 0
      StrCpy ${RESULT} "active"
      ; SERVICE_STATUS, seven DWORDs: dwCurrentState is the second,
      ; SERVICE_STOPPED is 1.
      System::Call '*(i 0, i 0, i 0, i 0, i 0, i 0, i 0) p .R7'
      System::Call 'advapi32::QueryServiceStatus(p R1, p R7) i .R8'
      System::Call '*$R7(i, i .R2)'
      System::Free $R7
      ${If} $R8 != 0
      ${AndIf} $R2 == 1
        StrCpy ${RESULT} "stopped"
      ${EndIf}
      System::Call 'advapi32::CloseServiceHandle(p R1)'
    ${EndIf}
    System::Call 'advapi32::CloseServiceHandle(p R0)'
  ${EndIf}
!macroend

; Stop the service and wait until it has stopped: orender answers the stop
; request at once and takes up to its 30 s to end. OK is "1" once it is
; stopped, "" when it could not be (no rights to stop it, or it never ended).
!macro StopService OK
  StrCpy ${OK} ""
  nsExec::ExecToLog 'sc.exe stop ${SERVICE_NAME}'
  Pop $R6
  StrCpy $R3 0
  ${Do}
    !insertmacro ServiceState $R4
    ${If} $R4 != "active"
      StrCpy ${OK} "1"
      ${ExitDo}
    ${EndIf}
    ; Refused (a per-user run cannot stop a service): waiting changes nothing.
    ${If} $R6 != 0
    ${AndIf} $R3 >= 4
      ${ExitDo}
    ${EndIf}
    ${If} $R3 > 80
      ${ExitDo}
    ${EndIf}
    Sleep 500
    IntOp $R3 $R3 + 1
  ${Loop}
!macroend

!define SERVICE_HINT "Stop it from Omniphony Studio, or with `sc stop ${SERVICE_NAME}` from an administrator prompt, then run this again."

; A file that exists and cannot be opened for writing is loaded by a running
; program: Windows would refuse to replace or delete it. A service stopped
; for the occasion goes back to running before the message.
!macro RefuseIfLocked FILE
  ${If} ${FileExists} "${FILE}"
    ClearErrors
    FileOpen $R0 "${FILE}" a
    ${If} ${Errors}
      ${If} $WasRunning == "1"
        nsExec::ExecToLog 'sc.exe start ${SERVICE_NAME}'
        Pop $R0
      ${EndIf}
      !insertmacro Fail "In use: ${FILE}. Close mpv, Omniphony Studio and the renderer, then run this again. A renderer running as a service: ${SERVICE_HINT}"
    ${EndIf}
    FileClose $R0
  ${EndIf}
!macroend

; ── Install ──────────────────────────────────────────────────────────────────

Section "Player, Studio and engine" SecCore
  SectionIn RO

  ; The renderer service of an earlier install, or Studio's, holds orender.exe.
  StrCpy $WasRunning ""
  !insertmacro ServiceState $0
  ${If} $0 == "active"
    !insertmacro StopService $1
    ${If} $1 != "1"
      !insertmacro Fail "The renderer service (${SERVICE_NAME}) is running and could not be stopped from here. ${SERVICE_HINT}"
    ${EndIf}
    StrCpy $WasRunning "1"
  ${EndIf}
  !insertmacro RefuseIfLocked "$INSTDIR\player\mpv.exe"
  !insertmacro RefuseIfLocked "$INSTDIR\studio\omniphony-studio-egui.exe"
  !insertmacro RefuseIfLocked "$INSTDIR\studio\orender.exe"
  !insertmacro RefuseIfLocked "$EngineDir\orender.dll"

  ; What the release this one replaces installed and this one no longer has.
  ; Only this product's own uninstaller, the one the registry names for this
  ; folder: any other uninstall.exe found there is someone else's.
  ${If} $Machine == "1"
    ReadRegStr $0 HKLM "${UNINST_KEY}" "InstallLocation"
  ${Else}
    ReadRegStr $0 HKCU "${UNINST_KEY}" "InstallLocation"
  ${EndIf}
  ${If} $0 == "$INSTDIR"
  ${AndIf} ${FileExists} "$INSTDIR\uninstall.exe"
    ExecWait '"$INSTDIR\uninstall.exe" /S /UPGRADE _?=$INSTDIR'
  ${EndIf}

  !include "${STAGE}/install-files.nsh"

  ; The engine in the folder every host searches before its own copy: the
  ; bytes Studio deploys there on its first start.
  CreateDirectory "$EngineDir"
  CopyFiles /SILENT "$INSTDIR\studio\engine\*.*" "$EngineDir"
  ${If} $Machine == "1"
    ; A service has no per-user folder: the engine next to the player as well.
    CopyFiles /SILENT "$INSTDIR\studio\engine\*.*" "$INSTDIR\player"
  ${EndIf}

  ; The engine's config directory, for every account and for a service. Made
  ; ahead with the grant Studio's service install applies, so a second account
  ; can write the file the first one created. No config.yaml is written.
  CreateDirectory "$ConfigDir"
  nsExec::ExecToLog 'icacls.exe "$ConfigDir" /grant *S-1-5-32-545:(OI)(CI)M'
  Pop $0

  WriteUninstaller "$INSTDIR\uninstall.exe"
  ${If} $Machine == "1"
    WriteRegStr HKLM "${UNINST_KEY}" "DisplayName" "${PRODUCT}"
    WriteRegStr HKLM "${UNINST_KEY}" "DisplayVersion" "${TAG}"
    WriteRegStr HKLM "${UNINST_KEY}" "Publisher" "Omniphony"
    WriteRegStr HKLM "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
    WriteRegStr HKLM "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
    WriteRegStr HKLM "${UNINST_KEY}" "EngineDir" "$EngineDir"
    WriteRegDWORD HKLM "${UNINST_KEY}" "NoModify" 1
    WriteRegDWORD HKLM "${UNINST_KEY}" "NoRepair" 1
  ${Else}
    WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "${PRODUCT}"
    WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${TAG}"
    WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "Omniphony"
    WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
    WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
    WriteRegStr HKCU "${UNINST_KEY}" "EngineDir" "$EngineDir"
    WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
    WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ${EndIf}
SectionEnd

Section "Start menu shortcuts" SecShortcuts
  CreateDirectory "$SMPROGRAMS\${PRODUCT}"
  ; A shortcut starts in the output folder of the moment.
  SetOutPath "$INSTDIR\studio"
  CreateShortCut "$SMPROGRAMS\${PRODUCT}\Omniphony Studio.lnk" "$INSTDIR\studio\omniphony-studio-egui.exe"
  ; The player's own default decoders are left alone: this entry asks for
  ; Omniphony's.
  SetOutPath "$INSTDIR\player"
  CreateShortCut "$SMPROGRAMS\${PRODUCT}\mpv-omniphony.lnk" "$INSTDIR\player\mpv.exe" "--ad=orender --player-operation-mode=pseudo-gui"
  SetOutPath "$INSTDIR"
  CreateShortCut "$SMPROGRAMS\${PRODUCT}\Uninstall Omniphony.lnk" "$INSTDIR\uninstall.exe"
SectionEnd

; In the user's session: the same audio devices as the player, the same
; per-user engine folder. A minimized console window, since orender is a
; console program; closing it stops the renderer.
Section /o "Start the renderer when I log on" SecStartup
  SetOutPath "$INSTDIR\studio"
  CreateShortCut "$SMSTARTUP\${STARTUP_LINK}" "$INSTDIR\studio\orender.exe" "${RENDER_ARGS}" "" 0 SW_SHOWMINIMIZED
  SetOutPath "$INSTDIR"
SectionEnd

; Unticked in the wizard on an upgrade: the logon start goes. A silent upgrade
; finds it ticked when it exists (.onInit) and keeps it.
Section "-Logon start off"
  ${IfNot} ${SectionIsSelected} ${SecStartup}
    Delete "$SMSTARTUP\${STARTUP_LINK}"
  ${EndIf}
SectionEnd

; Per machine with /SERVICE only. The service Studio's "Install service"
; creates: same name, same command line.
Section "-Renderer service"
  ${If} $Service != ""
    !insertmacro ServiceState $0
    ${If} $0 != ""
      nsExec::ExecToLog 'sc.exe config ${SERVICE_NAME} binPath= "\"$INSTDIR\studio\orender.exe\" ${RENDER_ARGS}" start= $Service DisplayName= "Omniphony Renderer"'
    ${Else}
      nsExec::ExecToLog 'sc.exe create ${SERVICE_NAME} binPath= "\"$INSTDIR\studio\orender.exe\" ${RENDER_ARGS}" start= $Service DisplayName= "Omniphony Renderer"'
    ${EndIf}
    Pop $0
    ${If} $0 != 0
      !insertmacro Fail "The renderer service could not be installed (sc.exe returned $0)."
    ${EndIf}
    nsExec::ExecToLog 'sc.exe description ${SERVICE_NAME} "Omniphony spatial audio renderer"'
    Pop $0
    WriteRegStr HKLM "${UNINST_KEY}" "Service" "1"
  ${EndIf}
  ; A service stopped for the upgrade goes back to running.
  ${If} $WasRunning == "1"
    nsExec::ExecToLog 'sc.exe start ${SERVICE_NAME}'
    Pop $0
  ${EndIf}
SectionEnd

; The decoder bridge is installed apart. Say where it goes, and whether the
; engine accepts the one that is there.
Section "-Decoder bridge report"
  ${If} $Machine == "1"
    StrCpy $2 "$INSTDIR\player and $INSTDIR\studio"
  ${Else}
    StrCpy $2 "$EngineDir"
  ${EndIf}
  StrCpy $BridgeReport "The decoder bridge is not part of this installer. Put its library into $2, next to the engine."
  ; `check-bridges` came with the `bridge_api:` line of `--version`: an older
  ; orender would take the word for an input to render.
  nsExec::ExecToStack '"$INSTDIR\studio\orender.exe" --version'
  Pop $0
  Pop $1
  ClearErrors
  ${WordFind} "$1" "bridge_api:" "E+1{" $3
  ${IfNot} ${Errors}
    nsExec::ExecToStack '"$INSTDIR\studio\orender.exe" check-bridges'
    Pop $0
    Pop $1
    DetailPrint "orender check-bridges:"
    DetailPrint "$1"
    ${If} $0 == 0
      StrCpy $BridgeReport "Decoder bridge: found, and accepted by this engine.$\r$\n$\r$\n$1"
    ${Else}
      StrCpy $BridgeReport "No usable decoder bridge yet: films play through mpv's own decoders, not through Omniphony.$\r$\n$\r$\nPut the bridge library into $2, next to the engine.$\r$\n$\r$\n$1"
    ${EndIf}
  ${EndIf}
  DetailPrint "$BridgeReport"
SectionEnd

; After the sections: it selects one by its index.
Function .onInit
  ${GetParameters} $0
  ClearErrors
  ${GetOptions} "$0" "/ALLUSERS" $1
  ${IfNot} ${Errors}
    StrCpy $Machine "1"
  ${EndIf}
  ClearErrors
  ${GetOptions} "$0" "/SERVICE" $1
  ${IfNot} ${Errors}
    ${If} $1 == "=auto"
      StrCpy $Service "auto"
    ${Else}
      StrCpy $Service "demand"
    ${EndIf}
    ${If} $Machine != "1"
      MessageBox MB_OK|MB_ICONSTOP "/SERVICE needs /ALLUSERS: a Windows service cannot read a per-user install." /SD IDOK
      SetErrorLevel 2
      Quit
    ${EndIf}
  ${EndIf}
  ClearErrors
  ${GetOptions} "$0" "/STARTUP" $1
  ${IfNot} ${Errors}
    !insertmacro SelectSection ${SecStartup}
  ${EndIf}

  !insertmacro ElevateIfMachine "$EXEPATH" "1"
  !insertmacro Folders
  ; An upgrade shows the logon start as it is.
  ${If} ${FileExists} "$SMSTARTUP\${STARTUP_LINK}"
    !insertmacro SelectSection ${SecStartup}
  ${EndIf}

  ; An earlier install says where it is; /D= wins over it.
  ${If} $INSTDIR == ""
    ${If} $Machine == "1"
      ReadRegStr $INSTDIR HKLM "${UNINST_KEY}" "InstallLocation"
    ${Else}
      ReadRegStr $INSTDIR HKCU "${UNINST_KEY}" "InstallLocation"
    ${EndIf}
  ${EndIf}
  ${If} $INSTDIR == ""
    ${If} $Machine == "1"
      StrCpy $INSTDIR "$PROGRAMFILES64\${PRODUCT}"
    ${Else}
      StrCpy $INSTDIR "$LOCALAPPDATA\Programs\${PRODUCT}"
    ${EndIf}
  ${EndIf}
FunctionEnd

; ── Uninstall ────────────────────────────────────────────────────────────────

Var Upgrade   ; "1": called by a newer installer, program files only

Function un.onInit
  SetRegView 64
  ReadRegStr $0 HKLM "${UNINST_KEY}" "InstallLocation"
  ${If} $0 == "$INSTDIR"
    StrCpy $Machine "1"
  ${EndIf}
  ${GetParameters} $0
  ClearErrors
  ${GetOptions} "$0" "/UPGRADE" $1
  ${IfNot} ${Errors}
    StrCpy $Upgrade "1"
  ${EndIf}
  !insertmacro ElevateIfMachine "$INSTDIR\uninstall.exe" "0"
  !insertmacro Folders
  ${If} $Machine == "1"
    ReadRegStr $0 HKLM "${UNINST_KEY}" "EngineDir"
  ${Else}
    ReadRegStr $0 HKCU "${UNINST_KEY}" "EngineDir"
  ${EndIf}
  ${If} $0 != ""
    StrCpy $EngineDir $0
  ${EndIf}
FunctionEnd

Section "Uninstall"
  ${If} $Upgrade != "1"
    ; The service this installer created, stopped before its binary goes.
    ReadRegStr $0 HKLM "${UNINST_KEY}" "Service"
    ${If} $Machine == "1"
    ${AndIf} $0 == "1"
      !insertmacro StopService $1
      ${If} $1 != "1"
        !insertmacro Fail "The renderer service (${SERVICE_NAME}) could not be stopped. ${SERVICE_HINT}"
      ${EndIf}
    ${EndIf}
    ; Nothing is removed while a program of this install runs: a file left
    ; behind would have no uninstaller any more. That includes a renderer
    ; started by the logon shortcut, by Studio, or by a service Studio made.
    !insertmacro RefuseIfLocked "$INSTDIR\player\mpv.exe"
    !insertmacro RefuseIfLocked "$INSTDIR\studio\omniphony-studio-egui.exe"
    !insertmacro RefuseIfLocked "$INSTDIR\studio\orender.exe"
    !insertmacro RefuseIfLocked "$EngineDir\orender.dll"
    !insertmacro RefuseIfLocked "$INSTDIR\player\orender.dll"
    ${If} $Machine == "1"
    ${AndIf} $0 == "1"
      nsExec::ExecToLog 'sc.exe delete ${SERVICE_NAME}'
      Pop $0
    ${EndIf}
    Delete "$SMSTARTUP\${STARTUP_LINK}"
    Delete "$SMPROGRAMS\${PRODUCT}\Omniphony Studio.lnk"
    Delete "$SMPROGRAMS\${PRODUCT}\mpv-omniphony.lnk"
    Delete "$SMPROGRAMS\${PRODUCT}\Uninstall Omniphony.lnk"
    RMDir "$SMPROGRAMS\${PRODUCT}"
  ${EndIf}

  ; The engine copies, named as the ones in studio\engine: the per-user
  ; folder's (Studio's deployed copy is the same file) and, per machine, the
  ; one next to the player.
  FindFirst $0 $1 "$INSTDIR\studio\engine\*.*"
  ${DoWhile} $1 != ""
    ${If} $1 != "."
    ${AndIf} $1 != ".."
      ${If} $Upgrade != "1"
        Delete "$EngineDir\$1"
      ${EndIf}
      ${If} $Machine == "1"
        Delete "$INSTDIR\player\$1"
      ${EndIf}
    ${EndIf}
    FindNext $0 $1
  ${Loop}
  FindClose $0

  ; Exactly the files this release installed: a decoder bridge put next to
  ; them, and its folder, stay.
  !include "${STAGE}/uninstall-files.nsh"

  ${If} $Upgrade != "1"
    RMDir "$EngineDir"
    Delete "$INSTDIR\uninstall.exe"
    RMDir "$INSTDIR"
    ${If} $Machine == "1"
      DeleteRegKey HKLM "${UNINST_KEY}"
    ${Else}
      DeleteRegKey HKCU "${UNINST_KEY}"
    ${EndIf}
    ; Kept: %ProgramData%\omniphony (config.yaml and its backups), Studio's
    ; own settings, mpv.conf, logs.
  ${EndIf}
SectionEnd
