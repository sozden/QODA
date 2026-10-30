<#
.SYNOPSIS
    Builds the Windows release zip of QODA.

.DESCRIPTION
    Compiles QODA, collects SoapySDR with the RTL-SDR and HackRF modules,
    libiio and the Visual C++ runtime from a PothosSDR installation, the
    RTL-SDR Blog driver, third-party license texts and optionally FFmpeg,
    and writes dist\QODA-<version>-windows-x64.zip.

.PARAMETER PothosDir
    PothosSDR installation (SoapySDR, modules, libiio). Default: C:\Program Files\PothosSDR

.PARAMETER RtlSdrBlogDir
    The x64 folder of an RTL-SDR Blog Windows release. If omitted, release
    V1.4.0 is downloaded from GitHub.

.PARAMETER FfmpegDir
    A shared FFmpeg build (folder with bin\ffmpeg.exe, bin\ffplay.exe and the
    DLLs) for the -ffmpeg zip. If omitted, $FfmpegBuild is downloaded from
    BtbN/FFmpeg-Builds on GitHub.

.PARAMETER FfmpegBuild
    BtbN release asset name without .zip.

.PARAMETER NoFfmpeg
    Write only the zip without FFmpeg.

.OUTPUTS
    dist\QODA-<version>-windows-x64.zip (QODA uses an FFmpeg installed on the
    system) and dist\QODA-<version>-windows-x64-ffmpeg.zip (FFmpeg bundled).

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\windows\package.ps1
#>
param(
    [string]$PothosDir = 'C:\Program Files\PothosSDR',
    [string]$RtlSdrBlogDir = '',
    [string]$FfmpegDir = '',
    [string]$FfmpegBuild = 'ffmpeg-n8.1-latest-win64-gpl-shared-8.1',
    [switch]$NoFfmpeg
)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..\..')
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"').Matches[0].Groups[1].Value
$name = "QODA-$version-windows-x64"
$dist = Join-Path $root 'dist'
$stage = Join-Path $dist $name
Write-Host "Packaging $name"

# 1. Build (soapysdr-sys looks for SoapySDR.dll in PATH).
$env:Path = (Join-Path $PothosDir 'bin') + ';' + $env:Path
cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

# 2. RTL-SDR Blog driver: PothosSDR's old rtlsdr.dll crashes when the stream is stopped.
if (-not $RtlSdrBlogDir) {
    $tmp = Join-Path $env:TEMP 'qoda-rtlsdrblog'
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory $tmp | Out-Null
    $zip = Join-Path $tmp 'Release.zip'
    Invoke-WebRequest 'https://github.com/rtlsdrblog/rtl-sdr-blog/releases/download/V1.4.0/Release.zip' -OutFile $zip
    Expand-Archive $zip -DestinationPath $tmp
    $RtlSdrBlogDir = Join-Path $tmp 'x64'
}

# 3. Collect the files.
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
$modules = Join-Path $stage 'lib\SoapySDR\modules0.8'
New-Item -ItemType Directory $modules -Force | Out-Null
function Copy-Required([string]$from, [string]$to) {
    if (-not (Test-Path $from)) { throw "missing: $from" }
    Copy-Item $from $to
}
foreach ($f in 'qoda.exe', 'qoda-analyze.exe') { Copy-Required "target\release\$f" $stage }
$pothosBin = Join-Path $PothosDir 'bin'
foreach ($f in 'SoapySDR.dll', 'hackrf.dll', 'libusb-1.0.dll', 'libiio.dll', 'MSVCP140.dll', 'VCRUNTIME140.dll', 'VCRUNTIME140_1.dll') {
    Copy-Required (Join-Path $pothosBin $f) $stage
}
foreach ($f in 'rtlsdr.dll', 'pthreadVC2.dll', 'msvcr100.dll') { Copy-Required (Join-Path $RtlSdrBlogDir $f) $stage }
$pothosModules = Join-Path $PothosDir 'lib\SoapySDR\modules0.8'
foreach ($f in 'rtlsdrSupport.dll', 'HackRFSupport.dll') { Copy-Required (Join-Path $pothosModules $f) $modules }

# 4. Documents and licenses.
foreach ($f in 'LICENSE', 'README.md') { Copy-Required $f $stage }
Copy-Required 'tools\windows\THIRD_PARTY_NOTICES.md' $stage
$licenses = Join-Path $stage 'licenses'
$pothosLicenses = Join-Path $PothosDir 'licenses'
foreach ($dir in 'SoapySDR', 'SoapyRTLSDR', 'SoapyHackRF', 'rtl-sdr', 'hackRF', 'libusb', 'libiio', 'Pthreads') {
    $src = Join-Path $pothosLicenses $dir
    if (-not (Test-Path $src)) { throw "missing license folder: $src" }
    Copy-Item $src (Join-Path $licenses $dir) -Recurse
}

# 5. Zip without FFmpeg.
function Write-Zip([string]$zipName) {
    $zipPath = Join-Path $dist $zipName
    Remove-Item $zipPath -Force -ErrorAction SilentlyContinue
    Compress-Archive -Path $stage -DestinationPath $zipPath
    $size = [math]::Round((Get-Item $zipPath).Length / 1MB, 1)
    Write-Host "Wrote $zipPath ($size MB)"
}
Write-Zip "$name.zip"
if ($NoFfmpeg) { return }

# 6. Zip with FFmpeg: BtbN's shared (DLL) GPL build, much smaller than the 228 MB static build.
if (-not $FfmpegDir) {
    $tmp = Join-Path $env:TEMP 'qoda-ffmpeg'
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory $tmp | Out-Null
    $zip = Join-Path $tmp 'ffmpeg.zip'
    Invoke-WebRequest "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/$FfmpegBuild.zip" -OutFile $zip
    Expand-Archive $zip -DestinationPath $tmp
    $FfmpegDir = Join-Path $tmp $FfmpegBuild
}
$ffBin = if (Test-Path (Join-Path $FfmpegDir 'bin')) { Join-Path $FfmpegDir 'bin' } else { $FfmpegDir }
$ff = Join-Path $stage 'ffmpeg'
New-Item -ItemType Directory $ff | Out-Null
foreach ($f in 'ffmpeg.exe', 'ffplay.exe') { Copy-Required (Join-Path $ffBin $f) $ff }
Get-ChildItem $ffBin -Filter *.dll | Copy-Item -Destination $ff
$ffLicense = Join-Path $FfmpegDir 'LICENSE.txt'
if (Test-Path $ffLicense) { Copy-Item $ffLicense (Join-Path $ff 'LICENSE.txt') }
$ffVersion = (& (Join-Path $ff 'ffmpeg.exe') -hide_banner -version | Select-Object -First 3) -join "`r`n"
Set-Content (Join-Path $ff 'README.txt') "FFmpeg bundled with QODA, licensed under GPL-3.0-or-later.`r`nBuild: https://github.com/BtbN/FFmpeg-Builds`r`nSource: https://ffmpeg.org/download.html`r`n`r`n$ffVersion" -Encoding utf8
Write-Zip "$name-ffmpeg.zip"
