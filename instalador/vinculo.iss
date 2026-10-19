; Instalador do Vínculo (Inno Setup 6)
; Instala por usuário em %LOCALAPPDATA%\Programs\Vinculo — sem pedir admin,
; o que também permite ao app se auto-atualizar trocando o próprio executável.

#define Versao GetEnv("VINCULO_VERSAO")
#if Versao == ""
  #define Versao "0.0.0"
#endif

[Setup]
AppId={{7E3B9C41-5A2D-4F7B-9D1E-VINCULO00001}
AppName=Vínculo
AppVersion={#Versao}
AppPublisher=Vínculo
AppPublisherURL=https://github.com/lucaoskaique/vinculo
DefaultDirName={userpf}\Vinculo
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputBaseFilename=vinculo-setup-windows
SetupIconFile=..\crates\app\assets\vinculo.ico
UninstallDisplayIcon={app}\vinculo.exe
WizardStyle=modern
ShowLanguageDialog=no

[Languages]
Name: "brazilianportuguese"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"

[Tasks]
Name: "desktopicon"; Description: "Criar atalho na área de trabalho"; GroupDescription: "Atalhos:"

[Files]
Source: "..\target\release\vinculo.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\Vínculo"; Filename: "{app}\vinculo.exe"
Name: "{userdesktop}\Vínculo"; Filename: "{app}\vinculo.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\vinculo.exe"; Description: "Abrir o Vínculo"; Flags: nowait postinstall skipifsilent
