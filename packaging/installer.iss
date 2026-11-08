; Inno Setup script for MicWright. Built by build.ps1:
;   ISCC.exe /DAppVersion=1.0.0 /DSourceDir=<published folder> packaging\installer.iss
; Installs per user (no admin) into %LOCALAPPDATA%\Programs unless "all users" is chosen.

#ifndef AppVersion
  #define AppVersion "1.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\dist\MicWright-" + AppVersion + "-win-x64"
#endif
#define AppExe "MicWright.exe"
#define RunKey "Software\Microsoft\Windows\CurrentVersion\Run"
#define RunValue "MicWright"
; Names from before the rename (QuadCast Enhancer), cleaned up on upgrade.
#define LegacyExe "QuadCastEnhancer.exe"
#define LegacyRunValue "QuadCast Enhancer"
#define CableUrl "https://vb-audio.com/Cable/"

[Setup]
AppId={{6AB9F101-DD31-4370-BDDC-C8534B1257B8}
AppName=MicWright
AppVersion={#AppVersion}
AppPublisher=Ryuu-x
AppComments=Real-time voice enhancement for the HyperX QuadCast
DefaultDirName={autopf}\MicWright
DefaultGroupName=MicWright
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.18362
OutputDir=..\dist
OutputBaseFilename=MicWright-{#AppVersion}-Setup
SetupIconFile=..\ui-winui\Assets\AppIcon.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName=MicWright
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
; The tray app is stopped in PrepareToInstall instead (it ignores a plain close on purpose).
CloseApplications=no

[Tasks]
Name: defaultmic; Description: "Make the enhanced voice the Windows &default microphone"; GroupDescription: "Microphone:"
Name: startup; Description: "&Start with Windows (hidden in the notification area)"; GroupDescription: "Microphone:"
Name: desktopicon; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[InstallDelete]
; Upgrading over QuadCast Enhancer reuses its folder: drop the old binaries.
Type: files; Name: "{app}\QuadCastEnhancer.*"
Type: files; Name: "{app}\qce_engine.dll"

[Icons]
Name: "{group}\MicWright"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\MicWright"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; Same value the app's own "Start with Windows" switch writes.
Root: HKCU; Subkey: "{#RunKey}"; ValueType: string; ValueName: "{#RunValue}"; ValueData: """{app}\{#AppExe}"" --background"; Tasks: startup

[Run]
Filename: "{app}\{#AppExe}"; Parameters: "--make-default"; Description: "Launch MicWright"; Flags: nowait postinstall skipifsilent runasoriginaluser; Tasks: defaultmic
Filename: "{app}\{#AppExe}"; Description: "Launch MicWright"; Flags: nowait postinstall skipifsilent runasoriginaluser; Tasks: not defaultmic

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM {#AppExe}"; Flags: runhidden; RunOnceId: "StopApp"
; Give apps their QuadCast back instead of a silent cable.
Filename: "{app}\{#AppExe}"; Parameters: "--restore-mic"; Flags: runhidden waituntilterminated; RunOnceId: "RestoreMic"

[Code]
var
  FinishedCableNote: TNewLinkLabel;

procedure CableLinkClick(Sender: TObject; const Link: String; LinkType: TSysLinkType);
var
  Code: Integer;
begin
  ShellExecAsOriginalUser('open', Link, '', '', SW_SHOWNORMAL, ewNoWait, Code);
end;

// Linked to VB-Audio's own page, not fetched here: their licence bars integrating the package
// in another installer without the author's agreement.
procedure InitializeWizard;
begin
  FinishedCableNote := TNewLinkLabel.Create(WizardForm);
  FinishedCableNote.AutoSize := False;
  FinishedCableNote.Left := WizardForm.FinishedLabel.Left;
  FinishedCableNote.Width := WizardForm.FinishedLabel.Width;
  FinishedCableNote.Caption := 'To send its enhanced audio to other apps on your device, MicWright needs VB-CABLE: ' +
    '<a href="{#CableUrl}">{#CableUrl}</a>';
  FinishedCableNote.UseVisualStyle := HighContrastActive;
  FinishedCableNote.OnLinkClick := @CableLinkClick;
  FinishedCableNote.Parent := WizardForm.FinishedPage;
  FinishedCableNote.AdjustHeight;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  // The run list stretches to the bottom of the page, so the note goes above it and the list moves down.
  if CurPageID = wpFinished then
  begin
    FinishedCableNote.Top := WizardForm.FinishedLabel.Top + WizardForm.FinishedLabel.Height + ScaleY(12);
    WizardForm.RunList.Top := FinishedCableNote.Top + FinishedCableNote.Height + ScaleY(12);
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  // Upgrading: stop the running copy so its files can be replaced.
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM {#AppExe}', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM {#LegacyExe}', '', SW_HIDE, ewWaitUntilTerminated, Code);
  RegDeleteValue(HKCU, '{#RunKey}', '{#LegacyRunValue}');
  Result := '';
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  // The app's own switch may have added it even if the installer didn't.
  if CurUninstallStep = usPostUninstall then
    RegDeleteValue(HKCU, '{#RunKey}', '{#RunValue}');
end;
