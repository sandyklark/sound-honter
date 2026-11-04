# Smoke-tests a built installer on this machine: installs silently for the current user, clicks the
# real right-click menu with real selections, uninstalls, and checks that nothing was left behind.
#
# It briefly changes the real right-click menu and Send To folder, restoring any existing Send To
# shortcuts of the same names afterwards. Dialogs will pop up on screen while it runs.
#
#   scripts\test-installer.ps1 [-Installer <path>] [-ScreenshotDir <folder>]

param([string]$Installer, [string]$ScreenshotDir)

$ErrorActionPreference = 'Stop'
$Root = Resolve-Path (Join-Path $PSScriptRoot '..')
if (-not $Installer) {
    $Installer = (Get-ChildItem (Join-Path $Root 'dist\SoundHonter-*-Setup.exe') | Sort-Object LastWriteTime | Select-Object -Last 1).FullName
}

$Failures = 0
function Check {
    param([string]$Name, [bool]$Passed)
    if ($Passed) { Write-Host "  ok    $Name" } else { Write-Host "  FAIL  $Name"; $script:Failures++ }
}

$Work = Join-Path ([IO.Path]::GetTempPath()) ("soundhonter-smoke-" + [IO.Path]::GetRandomFileName())
$InstallDir = Join-Path $Work 'install'
$Audio = Join-Path $Work 'audio & sounds!'
$Backup = Join-Path $Work 'sendto-backup'
New-Item -ItemType Directory $Audio, $Backup | Out-Null
Copy-Item (Join-Path $Root 'tests\fixtures\*.wav') $Audio

$SendTo = [Environment]::GetFolderPath('SendTo')
$Links = 'Sound Honter Mix.lnk', 'Sound Honter Convert to MP3.lnk'
foreach ($Link in $Links) {
    if (Test-Path (Join-Path $SendTo $Link)) { Copy-Item (Join-Path $SendTo $Link) $Backup }
}

Add-Type -Path (Join-Path $PSScriptRoot 'ShellMenu.cs') -ReferencedAssemblies System.Core, System.Drawing

function Get-AppWindow {
    $App = Get-Process soundhonter -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
    if ($App) { $App.MainWindowHandle } else { [IntPtr]::Zero }
}

function Wait-Until {
    param([scriptblock]$Condition, [int]$Seconds = 30)
    $Deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $Deadline) {
        if (& $Condition) { return $true }
        Start-Sleep -Milliseconds 100
    }
    $false
}

# Waits for the result dialog, photographs it if requested, and closes it.
function Close-AppDialog {
    param([string]$Screenshot)
    # Closing the progress dialog would cancel the job, so wait until it has given way to the result.
    $Found = Wait-Until {
        $Window = Get-AppWindow
        ($Window -ne [IntPtr]::Zero) -and -not [AppWindow]::HasButton($Window, 'Cancel')
    }
    if ($Found) {
        Start-Sleep -Milliseconds 300
        $Window = Get-AppWindow
        if ($ScreenshotDir -and $Screenshot) {
            New-Item -ItemType Directory -Force $ScreenshotDir | Out-Null
            [AppWindow]::Capture($Window, (Join-Path $ScreenshotDir "$Screenshot.png"))
        }
        [AppWindow]::Close($Window)
        $Found = Wait-Until { -not (Get-Process soundhonter -ErrorAction SilentlyContinue) }
    }
    $Found
}

function Find-Audio { param([string]$Filter) @(Get-ChildItem -LiteralPath $Audio -Filter $Filter -Force) }

$MenuKey = 'HKCU:\Software\Classes\SystemFileAssociations\audio\shell\SoundHonter'

# Splits a registered command line, and runs it with --no-ui and the given file substituted in.
function Invoke-Registered {
    param([string]$CommandLine, [string]$File)
    if ($CommandLine -notmatch '^"([^"]+)" (.+) "%1"$') { throw "Unexpected command: $CommandLine" }
    $Exe = $Matches[1]
    $Process = Start-Process $Exe -ArgumentList "$($Matches[2]) --no-ui `"$File`"" -Wait -PassThru -WindowStyle Hidden
    $Process.ExitCode
}

try {
    Write-Host "Installing $Installer"
    $Process = Start-Process $Installer -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/DIR=`"$InstallDir`"" -Wait -PassThru
    Check 'installer exits cleanly' ($Process.ExitCode -eq 0)

    Write-Host 'Installed files'
    foreach ($File in 'soundhonter.exe', 'THIRD-PARTY-NOTICES.txt', 'ffmpeg\ffmpeg.exe', 'ffmpeg\LICENSE.txt') {
        Check $File (Test-Path (Join-Path $InstallDir $File))
    }
    $Version = (Get-Item (Join-Path $InstallDir 'soundhonter.exe')).VersionInfo
    Check "exe version info ($($Version.ProductVersion))" ($Version.ProductName -eq 'Sound Honter')

    Write-Host 'Right-click menu'
    Check 'menu key exists' (Test-Path $MenuKey)
    $Verbs = Get-ChildItem "$MenuKey\shell"
    Check 'has every menu entry' ($Verbs.Count -eq 9)
    Check 'submenu is declared' ((Get-ItemProperty $MenuKey).PSObject.Properties.Name -contains 'SubCommands')

    $Drum = Join-Path $Audio 'drum.wav'
    $Blunt = Join-Path $Audio 'blunt.wav'
    $Selection = [string[]]@($Drum, $Blunt)
    $Wanted = 'Convert to MP3', 'Convert to WAV', 'Convert to FLAC', 'Convert to OGG', 'Convert to M4A', 'Normalize loudness', 'Trim silence', 'Mix selected files', 'Mix with options...' | ForEach-Object { "Sound Honter > $_" }
    foreach ($Files in @{ Name = 'one file'; Paths = [string[]]@($Drum) }, @{ Name = 'two files'; Paths = $Selection }) {
        $Items = @([ShellMenu]::Items($Files.Paths) | Where-Object { $_ -like 'Sound Honter > *' })
        Check "menu is complete and in order for $($Files.Name)" (($Items -join '|') -eq ($Wanted -join '|'))
    }

    Write-Host 'Running the registered commands'
    foreach ($Verb in $Verbs) {
        if ($Verb.PSChildName -notmatch '^[1-5]0') { continue }
        $Format = $Verb.PSChildName.Substring(2)
        $Command = (Get-ItemProperty "$($Verb.PSPath)\command").'(default)'
        $Exit = Invoke-Registered $Command $Drum
        $Expected = if ($Format -eq 'wav') { 'drum (1).wav' } else { "drum.$Format" }
        Check "$Format via right-click command" (($Exit -eq 0) -and (Test-Path -LiteralPath (Join-Path $Audio $Expected)))
    }

    Write-Host 'Send To shortcuts'
    $WScript = New-Object -ComObject WScript.Shell
    $Mix = $WScript.CreateShortcut((Join-Path $SendTo 'Sound Honter Mix.lnk'))
    $Process = Start-Process $Mix.TargetPath -ArgumentList "$($Mix.Arguments) --no-ui `"$Blunt`" `"$Drum`"" -Wait -PassThru -WindowStyle Hidden
    Check 'mix via Send To' (($Process.ExitCode -eq 0) -and (Test-Path -LiteralPath (Join-Path $Audio 'blunt + drum mix.wav')))
    $Convert = $WScript.CreateShortcut((Join-Path $SendTo 'Sound Honter Convert to MP3.lnk'))
    Check 'convert shortcut targets the app' ($Convert.TargetPath -eq (Join-Path $InstallDir 'soundhonter.exe') -and $Convert.Arguments -eq 'convert mp3')

    Write-Host 'Clicking the real menu'
    [ShellMenu]::Click($Selection, 'Sound Honter > Convert to FLAC')
    Check 'convert: summary dialog appears and closes' (Close-AppDialog 'convert-summary')
    Check 'convert: both selected files became FLAC' ((Find-Audio '*.flac').Count -eq 3)

    Remove-Item -LiteralPath (Join-Path $Audio 'blunt + drum mix.wav')
    [ShellMenu]::Click($Selection, 'Sound Honter > Mix selected files')
    Check 'mix: summary dialog appears and closes' (Close-AppDialog 'mix-summary')
    Check 'mix: the selection became exactly one mix' ((Find-Audio '*mix*.wav').Count -eq 1)

    [ShellMenu]::Click([string[]]@($Drum), 'Sound Honter > Mix selected files')
    Check 'mix: one file explains itself' (Close-AppDialog 'mix-one-file')
    Check 'mix: one file makes nothing' ((Find-Audio '*mix*.wav').Count -eq 1)

    $Corrupt = Join-Path $Audio 'corrupt.wav'
    [IO.File]::WriteAllText($Corrupt, 'not audio')
    [ShellMenu]::Click([string[]]@($Corrupt, $Blunt), 'Sound Honter > Convert to OGG')
    Check 'partial failure dialog appears and closes' (Close-AppDialog 'partial-failure')
    [ShellMenu]::Click([string[]]@($Corrupt), 'Sound Honter > Convert to OGG')
    Check 'total failure dialog appears and closes' (Close-AppDialog 'total-failure')
    Check 'failures are logged' (Test-Path (Join-Path $env:LOCALAPPDATA 'Sound Honter\last-errors.log'))
    Remove-Item -LiteralPath $Corrupt

    # Explorer starts one process per selected file: a big selection must still become one job.
    $Many = 1..15 | ForEach-Object {
        $Copy = Join-Path $Audio ('many{0:d2}.wav' -f $_)
        Copy-Item -LiteralPath $Drum $Copy
        $Copy
    }
    [ShellMenu]::Click([string[]]$Many, 'Sound Honter > Mix selected files')
    Check '15 files: mix summary dialog appears and closes' (Close-AppDialog '')
    $Mixes = @(Find-Audio 'many*mix*.wav')
    Write-Host "  info  mixes made from 15 files: $($Mixes.Name -join ', ')"
    Check '15 files: exactly one mix of all of them' ($Mixes.Count -eq 1 -and $Mixes[0].Name -like '* + 14 more mix*')
    [ShellMenu]::Click([string[]]$Many, 'Sound Honter > Convert to M4A')
    Check '15 files: convert summary dialog appears and closes' (Close-AppDialog 'convert-15')
    Check '15 files: every file converted exactly once' ((Find-Audio 'many*.m4a').Count -eq 15 -and (Find-Audio 'many*(*.m4a').Count -eq 0)
    $Extra = Join-Path $Audio 'many16.wav'
    Copy-Item -LiteralPath $Drum $Extra
    $Items = @([ShellMenu]::Items([string[]]($Many + $Extra)) | Where-Object { $_ -like 'Sound Honter*' })
    Write-Host "  info  Sound Honter menu entries with 16 files selected: $($Items.Count)"
    Get-ChildItem -LiteralPath $Audio -Filter 'many*' | Remove-Item

    # An hour of audio takes long enough to see progress, and to cancel.
    $Long = Join-Path $Audio 'long.wav'
    & (Join-Path $InstallDir 'ffmpeg\ffmpeg.exe') -hide_banner -loglevel error -f lavfi -i 'sine=440:d=3600:r=44100' -y $Long
    [ShellMenu]::Click([string[]]@($Long), 'Sound Honter > Convert to FLAC')
    Check 'progress dialog appears' (Wait-Until { $Window = Get-AppWindow; ($Window -ne [IntPtr]::Zero) -and [AppWindow]::HasButton($Window, 'Cancel') })
    Start-Sleep -Milliseconds 1500
    if ($ScreenshotDir) { [AppWindow]::Capture((Get-AppWindow), (Join-Path $ScreenshotDir 'progress.png')) }
    [AppWindow]::Close((Get-AppWindow))
    Check 'cancel ends the app without a further dialog' (Wait-Until { -not (Get-Process soundhonter -ErrorAction SilentlyContinue) } 15)
    Check 'cancel stops FFmpeg' (-not (Get-Process ffmpeg -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$InstallDir*" }))
    Check 'cancel leaves no output or temp files' (((Find-Audio 'long.flac').Count -eq 0) -and ((Find-Audio '*.tmp').Count -eq 0))
    Remove-Item -LiteralPath $Long

    Write-Host 'About dialog'
    Start-Process (Join-Path $InstallDir 'soundhonter.exe')
    Check 'about dialog appears and closes' (Close-AppDialog 'about')

    Write-Host 'Reinstalling over the top'
    $Process = Start-Process $Installer -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait -PassThru
    Check 'reinstall exits cleanly' ($Process.ExitCode -eq 0)
    Check 'reinstall keeps the chosen folder' (Test-Path (Join-Path $InstallDir 'soundhonter.exe'))

    Write-Host 'Uninstalling'
    $Process = Start-Process (Join-Path $InstallDir 'unins000.exe') -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait -PassThru
    Check 'uninstaller exits cleanly' ($Process.ExitCode -eq 0)
    # The uninstaller hands off to a temporary copy of itself, so allow it a moment to finish.
    $Deadline = (Get-Date).AddSeconds(30)
    while ((Test-Path (Join-Path $InstallDir 'soundhonter.exe')) -and (Get-Date) -lt $Deadline) { Start-Sleep -Milliseconds 200 }
    Start-Sleep -Seconds 2
    Check 'files removed' (-not (Test-Path (Join-Path $InstallDir 'soundhonter.exe')))
    Check 'menu removed' (-not (Test-Path $MenuKey))
    Check 'menu gone from Explorer' (-not (@([ShellMenu]::Items([string[]]@($Drum))) -like 'Sound Honter*'))
    Check 'app data removed' (-not (Test-Path (Join-Path $env:LOCALAPPDATA 'Sound Honter')))
    foreach ($Link in $Links) { Check "$Link removed" (-not (Test-Path (Join-Path $SendTo $Link))) }
    Check 'user audio untouched' (Test-Path -LiteralPath $Drum)
}
finally {
    foreach ($Link in $Links) {
        if (Test-Path (Join-Path $Backup $Link)) { Copy-Item (Join-Path $Backup $Link) $SendTo -Force }
    }
    # If a check threw part-way, don't leave a test install registered.
    $Uninstaller = Join-Path $InstallDir 'unins000.exe'
    if (Test-Path $Uninstaller) { Start-Process $Uninstaller -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait }
    Remove-Item $Work -Recurse -Force -ErrorAction SilentlyContinue
}

if ($Failures -gt 0) { Write-Host "$Failures check(s) failed."; exit 1 }
Write-Host 'All installer checks passed.'
