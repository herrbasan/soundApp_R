param(
    [string]$Version = '',
    [switch]$SkipBuild,
    [switch]$NoPortable,
    [switch]$NoInstaller,
    [switch]$Publish
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not $Version) {
    $match = Select-String -Path (Join-Path $root 'crates\soundapp\Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
    if (-not $match) { throw 'could not read soundapp version from Cargo.toml' }
    $Version = $match.Matches[0].Groups[1].Value
}

# --- 1. Build + runtime distribution -----------------------------------------
if (-not $SkipBuild) {
    cargo build --release -p soundapp
    if ($LASTEXITCODE -ne 0) { throw 'cargo build --release failed' }
}
$distArgs = @{ NoZip = $true }
if ($env:SOUNDAPP_SOUNDFONT) { $distArgs.SoundFont = $env:SOUNDAPP_SOUNDFONT }
& (Join-Path $root 'scripts\make-dist.ps1') @distArgs

# --- 2. Installer (Inno Setup) -----------------------------------------------
$setup = $null
if (-not $NoInstaller) {
    & (Join-Path $root 'installer\Generate-Associations.ps1')
    $isccCandidates = @(
        $env:ISCC_PATH,
        (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'),
        'C:\Program Files (x86)\Inno Setup 6\ISCC.exe',
        'C:\Program Files\Inno Setup 6\ISCC.exe',
        (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 7\ISCC.exe'),
        'C:\Program Files (x86)\Inno Setup 7\ISCC.exe'
    )
    $iscc = $isccCandidates | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
    if (-not $iscc) { $iscc = (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source }
    if (-not $iscc) { throw 'ISCC.exe not found. Install Inno Setup 6 or set ISCC_PATH.' }
    $compiled = $false
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        & $iscc "/DMyAppVersion=$Version" (Join-Path $root 'installer\soundapp_r.iss')
        if ($LASTEXITCODE -eq 0) { $compiled = $true; break }
        Write-Output "Inno Setup attempt $attempt failed; retrying after resource-handle release..."
        Start-Sleep -Seconds 2
    }
    if (-not $compiled) { throw 'Inno Setup failed after 3 attempts' }
    $setup = Join-Path $root "dist\SoundAppR-$Version-setup.exe"
    if (-not (Test-Path $setup)) { throw "installer output missing: $setup" }
}

# --- 3. Portable archive ------------------------------------------------------
$portable = $null
if (-not $NoPortable) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $portable = Join-Path $root "dist\SoundAppR-$Version-portable.zip"
    if (Test-Path $portable) { Remove-Item $portable -Force }
    [System.IO.Compression.ZipFile]::CreateFromDirectory(
        (Join-Path $root 'dist\soundapp_r'),
        $portable,
        [System.IO.Compression.CompressionLevel]::Optimal,
        $false
    )
}

# --- 4. Checksums -------------------------------------------------------------
$files = @($setup, $portable) | Where-Object { $_ -and (Test-Path $_) }
$sums = Join-Path $root 'dist\SHA256SUMS.txt'
$lines = foreach ($file in $files) {
    $hash = (Get-FileHash $file -Algorithm SHA256).Hash.ToLower()
    "$hash  $(Split-Path -Leaf $file)"
}
[System.IO.File]::WriteAllLines($sums, $lines, (New-Object System.Text.UTF8Encoding $false))
$files += $sums

Write-Output "release $Version ready:"
foreach ($file in $files) {
    Write-Output ("  {0,10:N1} MB  {1}" -f ((Get-Item $file).Length / 1MB), $file)
}

if ($Publish) {
    $tag = if ($Version.StartsWith('v')) { $Version } else { "v$Version" }
    $assets = $files | ForEach-Object { $_ }
    & gh release view $tag *> $null
    if ($LASTEXITCODE -eq 0) {
        & gh release upload $tag @assets --clobber
    } else {
        & gh release create $tag @assets --title "SoundApp R $Version" --generate-notes
    }
    if ($LASTEXITCODE -ne 0) { throw "gh release publish failed (exit $LASTEXITCODE)" }
}
