# Downloads the FFmpeg build that ships with Sound Honter, verifying it against pinned checksums.
# To upgrade FFmpeg: update all three values below, then run the tests and update the notices.

$ErrorActionPreference = 'Stop'

$Version = '9.0.2'
$ZipSha256 = '60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba'
$ExeSha256 = '3256173f3f8bffd7df12227c68adf68025edb1832273a9530688a7bb1ed8edec'

$Target = Join-Path $PSScriptRoot '..\third_party\ffmpeg'
$Exe = Join-Path $Target 'ffmpeg.exe'

if ((Test-Path $Exe) -and (Get-FileHash $Exe -Algorithm SHA256).Hash -eq $ExeSha256) {
    Write-Host "FFmpeg $Version is present and verified."
    exit 0
}

$Work = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName())
New-Item -ItemType Directory $Work | Out-Null
try {
    $Zip = Join-Path $Work 'ffmpeg.zip'
    $Name = "ffmpeg-$Version-essentials_build"
    Write-Host "Downloading FFmpeg $Version..."
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest "https://www.gyan.dev/ffmpeg/builds/packages/$Name.zip" -OutFile $Zip -UseBasicParsing

    if ((Get-FileHash $Zip -Algorithm SHA256).Hash -ne $ZipSha256) {
        throw 'The FFmpeg download does not match its pinned checksum.'
    }
    Expand-Archive $Zip -DestinationPath $Work

    $Downloaded = Join-Path $Work "$Name\bin\ffmpeg.exe"
    if ((Get-FileHash $Downloaded -Algorithm SHA256).Hash -ne $ExeSha256) {
        throw 'The extracted ffmpeg.exe does not match its pinned checksum.'
    }
    New-Item -ItemType Directory -Force $Target | Out-Null
    Copy-Item $Downloaded $Exe -Force
    Copy-Item (Join-Path $Work "$Name\LICENSE") (Join-Path $Target 'LICENSE.txt') -Force
    Write-Host "FFmpeg $Version verified and installed to third_party\ffmpeg."
}
finally {
    Remove-Item $Work -Recurse -Force
}
