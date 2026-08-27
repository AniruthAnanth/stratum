; Stratum — NSIS installer hooks (design 08 §6.3, ADR-011).
; Wired via bundle.windows.nsis.installerHooks in tauri.windows.conf.json.
;
; THE RULE: offer, do not seize. OpenWithProgids adds Stratum to "Open with";
; it does NOT change the current default. We NEVER write the (Default) value
; of Software\Classes\.do or .dta — smoke.yml asserts it is still null after
; install. Taking over is an explicit, reversible user action inside Stratum
; (Settings → General → File Associations).

!macro NSIS_HOOK_POSTINSTALL
  ; ProgIDs — declare capability
  WriteRegStr SHCTX "Software\Classes\Stratum.DoFile" "" "Stata Do-file"
  WriteRegStr SHCTX "Software\Classes\Stratum.DoFile\DefaultIcon" "" "$INSTDIR\Stratum.exe,1"
  WriteRegStr SHCTX "Software\Classes\Stratum.DoFile\shell\open\command" "" '"$INSTDIR\Stratum.exe" "%1"'
  WriteRegStr SHCTX "Software\Classes\Stratum.DtaFile" "" "Stata Dataset"
  WriteRegStr SHCTX "Software\Classes\Stratum.DtaFile\DefaultIcon" "" "$INSTDIR\Stratum.exe,2"
  WriteRegStr SHCTX "Software\Classes\Stratum.DtaFile\shell\open\command" "" '"$INSTDIR\Stratum.exe" "%1"'

  ; Offer, do not seize: OpenWithProgids only. The (Default) value of the
  ; extension keys is deliberately never written.
  WriteRegStr SHCTX "Software\Classes\.do\OpenWithProgids"  "Stratum.DoFile"  ""
  WriteRegStr SHCTX "Software\Classes\.dta\OpenWithProgids" "Stratum.DtaFile" ""

  ; Default Programs — makes Stratum selectable in Settings > Default apps
  WriteRegStr SHCTX "Software\Stratum\Capabilities" "ApplicationName"        "Stratum"
  WriteRegStr SHCTX "Software\Stratum\Capabilities" "ApplicationDescription" "Interactive statistical IDE"
  WriteRegStr SHCTX "Software\Stratum\Capabilities\FileAssociations" ".do"  "Stratum.DoFile"
  WriteRegStr SHCTX "Software\Stratum\Capabilities\FileAssociations" ".dta" "Stratum.DtaFile"
  WriteRegStr SHCTX "Software\RegisteredApplications" "Stratum" "Software\Stratum\Capabilities"

  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, i 0, i 0)'  ; SHCNE_ASSOCCHANGED
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Uninstall deletes exactly the keys the install wrote, and nothing else.
  ; `cargo xtask smoke assert-registry-clean` checks for leftovers.
  DeleteRegKey   SHCTX "Software\Classes\Stratum.DoFile"
  DeleteRegKey   SHCTX "Software\Classes\Stratum.DtaFile"
  DeleteRegValue SHCTX "Software\Classes\.do\OpenWithProgids"  "Stratum.DoFile"
  DeleteRegValue SHCTX "Software\Classes\.dta\OpenWithProgids" "Stratum.DtaFile"
  DeleteRegKey   SHCTX "Software\Stratum\Capabilities"
  DeleteRegKey /ifempty SHCTX "Software\Stratum"
  DeleteRegValue SHCTX "Software\RegisteredApplications" "Stratum"
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, i 0, i 0)'
!macroend
