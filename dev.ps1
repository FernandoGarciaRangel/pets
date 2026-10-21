# Sobe o Pets no terminal atual (sem abrir janela externa).
#   ./dev.ps1                  debug, com log dos hooks no terminal
#   ./dev.ps1 -Release         build otimizado
#   ./dev.ps1 -Install         build release + instala em %LOCALAPPDATA%\Programs\Pets (hooks e autostart apontam para lá)
#   ./dev.ps1 -- install-hooks argumentos repassados ao pets.exe
param(
    [switch]$Release,
    [switch]$Install,
    [Parameter(ValueFromRemainingArguments = $true)][string[]]$AppArgs
)
# Sem ErrorActionPreference=Stop: o cargo escreve progresso no stderr, e com Stop o PowerShell 5.1 trata isso como erro.
Set-Location $PSScriptRoot

if ($Install) {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    # O exe release é GUI: `&` não esperaria por ele, e Start-Process -Wait esperaria também pelo pet que ele abre.
    $psi = New-Object System.Diagnostics.ProcessStartInfo "$PSScriptRoot\target\release\pets.exe", 'install'
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.StandardOutputEncoding = [Text.Encoding]::UTF8
    $psi.StandardErrorEncoding = [Text.Encoding]::UTF8
    $p = [System.Diagnostics.Process]::Start($psi)
    $p.StandardOutput.ReadToEnd()
    $p.StandardError.ReadToEnd()
    $p.WaitForExit()
    exit $p.ExitCode
}

$cargoArgs = @('run')
if ($Release) { $cargoArgs += '--release' }
if ($AppArgs) { $cargoArgs += '--'; $cargoArgs += $AppArgs }
cargo @cargoArgs
