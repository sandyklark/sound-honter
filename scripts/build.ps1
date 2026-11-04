# Builds the release installer: dist\SoundHonter-<version>-Setup.exe
#
# Needs rustup (https://rustup.rs) and Inno Setup 6 (https://jrsoftware.org/isinfo.php).
#
#   scripts\build.ps1
#   scripts\build.ps1 -SkipTests
#   scripts\build.ps1 -SignCommand 'signtool sign /fd sha256 /tr http://timestamp.example /td sha256 /a $f'
#
# In -SignCommand, $f stands for the file to sign. Single-quote it so PowerShell leaves $f alone.

param(
    [switch]$SkipTests,
    [string]$SignCommand
)

$ErrorActionPreference = 'Stop'

# The GNU toolchain is self-contained: building needs neither Visual Studio nor the Windows SDK.
$Toolchain = '1.99.1-x86_64-pc-windows-gnu'
$Root = Resolve-Path (Join-Path $PSScriptRoot '..')

function Invoke-Checked {
    param([string]$Program, [string[]]$Arguments)
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Program $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
}

Push-Location $Root
try {
    $Iscc = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
    if (-not (Test-Path $Iscc)) { throw 'Inno Setup 6 is not installed.' }

    $Version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"$').Matches[0].Groups[1].Value

    Invoke-Checked rustup @('toolchain', 'install', $Toolchain, '--profile', 'minimal', '-c', 'clippy', '-c', 'rustfmt', '--no-self-update')
    & (Join-Path $PSScriptRoot 'fetch-ffmpeg.ps1')

    if (-not $SkipTests) {
        Invoke-Checked cargo @("+$Toolchain", 'fmt', '--check')
        Invoke-Checked cargo @("+$Toolchain", 'clippy', '--all-targets', '--', '-D', 'warnings')
        Invoke-Checked cargo @("+$Toolchain", 'test')
    }

    Invoke-Checked cargo @("+$Toolchain", 'build', '--release')
    Invoke-Checked cargo @("+$Toolchain", 'run', '--release', '--example', 'stamp', '--', 'target\release\soundhonter.exe', 'assets\icon.ico')

    $IsccArguments = @("/DAppVersion=$Version", '/Qp')
    if ($SignCommand) {
        $IsccArguments += '/DSign'
        $IsccArguments += "/Ssoundhonter=$SignCommand"
    }
    Invoke-Checked $Iscc ($IsccArguments + 'packaging\sound-honter.iss')

    $Installer = Join-Path $Root "dist\SoundHonter-$Version-Setup.exe"
    $Hash = (Get-FileHash $Installer -Algorithm SHA256).Hash
    Write-Host ''
    Write-Host "Built $Installer"
    Write-Host "SHA-256 $Hash"
    if (-not $SignCommand) { Write-Host 'Not code-signed: Windows SmartScreen will warn users. See docs\RELEASING.md.' }
}
finally {
    Pop-Location
}
