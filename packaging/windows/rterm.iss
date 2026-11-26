#ifndef AppVersion
  #define AppVersion "2.0.0"
#endif
#ifndef BinDir
  #define BinDir "..\..\target\release"
#endif
#ifndef OutDir
  #define OutDir "..\..\dist"
#endif

[Setup]
AppId=RTerm
AppName=RTerm
AppVersion={#AppVersion}
DefaultDirName={localappdata}\Programs\RTerm
DefaultGroupName=RTerm
DisableProgramGroupPage=yes
AllowNoIcons=no
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutDir}
OutputBaseFilename=RTerm-{#AppVersion}-windows-x64-setup
Compression=lzma2
SolidCompression=yes
ChangesEnvironment=yes
WizardStyle=modern
SetupIconFile=..\..\assets\rterm.ico
UninstallDisplayIcon={app}\rterm.exe
CloseApplications=yes
#ifdef SignedBuild
SignTool=RTermSign
SignedUninstaller=yes
#endif

[Files]
Source: "{#BinDir}\rterm.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\rterm-agent.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\assets\Inter-OFL.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "..\..\assets\JetBrainsMono-OFL.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "..\..\vendor\alacritty_terminal\LICENSE-APACHE"; DestDir: "{app}\licenses"; DestName: "alacritty-LICENSE-APACHE"; Flags: ignoreversion
Source: "..\..\target\release-notices\THIRD-PARTY-LICENSES.html"; DestDir: "{app}\licenses"; Flags: ignoreversion
#if FileExists("..\..\LICENSE")
Source: "..\..\LICENSE"; DestDir: "{app}\licenses"; DestName: "RTerm-LICENSE"; Flags: ignoreversion
#endif

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Icons]
Name: "{group}\RTerm"; Filename: "{app}\rterm.exe"; WorkingDir: "{app}"
Name: "{userdesktop}\RTerm"; Filename: "{app}\rterm.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Code]
// Keep the desktop and CLI together. Add just this installation directory
// to the current user's PATH, without replacing their existing entries.
const
  EnvKey = 'Environment';
  InstallerKey = 'Software\RTerm\Installer';

function PathWithoutEntry(const Original, Entry: String; var Found: Boolean): String;
var
  Remaining, Part: String;
  Separator: Integer;
begin
  Remaining := Original;
  Result := '';
  Found := False;
  while Remaining <> '' do
  begin
    Separator := Pos(';', Remaining);
    if Separator = 0 then
    begin
      Part := Remaining;
      Remaining := '';
    end
    else
    begin
      Part := Copy(Remaining, 1, Separator - 1);
      Delete(Remaining, 1, Separator);
    end;
    if CompareText(Trim(Part), Entry) = 0 then
      Found := True
    else if Part <> '' then
    begin
      if Result <> '' then Result := Result + ';';
      Result := Result + Part;
    end;
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Existing, Entry, Cleaned, Previous: String;
  Found, Removed: Boolean;
begin
  if CurStep <> ssPostInstall then Exit;
  Entry := ExpandConstant('{app}');
  Existing := '';
  RegQueryStringValue(HKCU, EnvKey, 'Path', Existing);
  if RegQueryStringValue(HKCU, InstallerKey, 'PathAdded', Previous)
     and (CompareText(Previous, Entry) <> 0) then
    Existing := PathWithoutEntry(Existing, Previous, Removed);
  Cleaned := PathWithoutEntry(Existing, Entry, Found);
  if not Found then
  begin
    if Existing <> '' then Existing := Existing + ';';
    Existing := Existing + Entry;
    if RegWriteExpandStringValue(HKCU, EnvKey, 'Path', Existing) then
      RegWriteStringValue(HKCU, InstallerKey, 'PathAdded', Entry)
    else
      MsgBox('RTerm was installed, but PATH could not be updated. Add the installation folder to your user PATH manually.', mbError, MB_OK);
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Existing, Entry, Cleaned: String;
  Found: Boolean;
begin
  if CurUninstallStep <> usUninstall then Exit;
  if RegQueryStringValue(HKCU, InstallerKey, 'PathAdded', Entry)
     and (CompareText(Entry, ExpandConstant('{app}')) = 0) then
  begin
    if RegQueryStringValue(HKCU, EnvKey, 'Path', Existing) then
    begin
      Cleaned := PathWithoutEntry(Existing, Entry, Found);
      if Found then RegWriteExpandStringValue(HKCU, EnvKey, 'Path', Cleaned);
    end;
    RegDeleteValue(HKCU, InstallerKey, 'PathAdded');
  end;
end;
