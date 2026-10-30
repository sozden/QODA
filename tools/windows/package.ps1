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
    Folder with ffmpeg.exe and ffplay.exe to bundle. If omitted, FFmpeg is not
    bundled and QODA uses an FFmpeg installed on the system.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\windows\package.ps1
#>
param(
    [string]$PothosDir = 'C:\Program Files\PothosSDR',
    [string]$RtlSdrBlogDir = '',
    [string]$FfmpegDir = ''
)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..\..')
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"').Matches[0].Groups[1].Value
$name = "QODA-$version-windows-x64"
$dist = Join-Path $root 'dist'
$stage = Join-Path $dist $name
Write-Host "Packaging $name"

# 1. Derleme (soapysdr-sys, SoapySDR.dll'i PATH'te arar).
$env:Path = (Join-Path $PothosDir 'bin') + ';' + $env:Path
cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

# 2. RTL-SDR Blog sürücüsü: PothosSDR'ın eski rtlsdr.dll'i akış durdurulurken çöküyor.
if (-not $RtlSdrBlogDir) {
    $tmp = Join-Path $env:TEMP 'qoda-rtlsdrblog'
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory $tmp | Out-Null
    $zip = Join-Path $tmp 'Release.zip'
    Invoke-WebRequest 'https://github.com/rtlsdrblog/rtl-sdr-blog/releases/download/V1.4.0/Release.zip' -OutFile $zip
    Expand-Archive $zip -DestinationPath $tmp
    $RtlSdrBlogDir = Join-Path $tmp 'x64'
}

# 3. Dosyaları topla.
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

if ($FfmpegDir) {
    $ff = Join-Path $stage 'ffmpeg'
    New-Item -ItemType Directory $ff | Out-Null
    foreach ($f in 'ffmpeg.exe', 'ffplay.exe') { Copy-Required (Join-Path $FfmpegDir $f) $ff }
    $ffVersion = (& (Join-Path $FfmpegDir 'ffmpeg.exe') -hide_banner -version | Select-Object -First 3) -join "`r`n"
    Set-Content (Join-Path $ff 'README.txt') "FFmpeg bundled with QODA, licensed under GPL-3.0-or-later.`r`nSource: https://ffmpeg.org/download.html`r`n`r`n$ffVersion" -Encoding utf8
}

# 4. Belgeler ve lisanslar.
foreach ($f in 'LICENSE', 'README.md', 'README.tr.md') { Copy-Required $f $stage }
Copy-Required 'tools\windows\THIRD_PARTY_NOTICES.md' $stage
$licenses = Join-Path $stage 'licenses'
$pothosLicenses = Join-Path $PothosDir 'licenses'
foreach ($dir in 'SoapySDR', 'SoapyRTLSDR', 'SoapyHackRF', 'rtl-sdr', 'hackRF', 'libusb', 'libiio', 'Pthreads') {
    $src = Join-Path $pothosLicenses $dir
    if (-not (Test-Path $src)) { throw "missing license folder: $src" }
    Copy-Item $src (Join-Path $licenses $dir) -Recurse
}

# 5. Zip.
$zipPath = Join-Path $dist "$name.zip"
Remove-Item $zipPath -Force -ErrorAction SilentlyContinue
Compress-Archive -Path $stage -DestinationPath $zipPath
$size = [math]::Round((Get-Item $zipPath).Length / 1MB, 1)
Write-Host "Wrote $zipPath ($size MB)"
