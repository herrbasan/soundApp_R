param(
    [string]$Root = (Split-Path -Parent $PSScriptRoot)
)

$ErrorActionPreference = 'Stop'
$releaseTag = 'deps-v1'
$assetName = 'soundapp-r-deps-v1.zip'
$expectedSha256 = '91727e2ec45736a655dd1b12fb43939a1c7d30e74e0dc7c18c393b16299f5204'
$deps = Join-Path $Root '.deps'
$archive = Join-Path $deps $assetName
New-Item -ItemType Directory -Path $deps -Force | Out-Null

if (-not (Test-Path $archive)) {
    $url = "https://github.com/herrbasan/soundApp_R/releases/download/$releaseTag/$assetName"
    Write-Output "downloading $url"
    Invoke-WebRequest -Uri $url -OutFile $archive -UseBasicParsing
}
$actual = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLower()
if ($actual -ne $expectedSha256) {
    throw "release dependency checksum mismatch: $actual"
}

$stage = Join-Path $deps 'release'
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
Expand-Archive -LiteralPath $archive -DestinationPath $stage

$ffmpeg = Join-Path $stage 'ffmpeg'
$soundfont = Join-Path $stage 'soundfonts\default.sf2'
foreach ($path in @(
    (Join-Path $ffmpeg 'include'),
    (Join-Path $ffmpeg 'lib'),
    (Join-Path $ffmpeg 'bin'),
    $soundfont
)) {
    if (-not (Test-Path $path)) { throw "release dependency missing after extract: $path" }
}

$values = [ordered]@{
    SOUNDAPP_FFMPEG_INCLUDE = (Join-Path $ffmpeg 'include')
    SOUNDAPP_FFMPEG_LIB = (Join-Path $ffmpeg 'lib')
    SOUNDAPP_FFMPEG_BIN = (Join-Path $ffmpeg 'bin')
    SOUNDAPP_SOUNDFONT = $soundfont
}
foreach ($item in $values.GetEnumerator()) {
    [Environment]::SetEnvironmentVariable($item.Key, $item.Value, 'Process')
    if ($env:GITHUB_ENV) {
        [System.IO.File]::AppendAllText(
            $env:GITHUB_ENV,
            "$($item.Key)=$($item.Value)`n",
            (New-Object System.Text.UTF8Encoding $false)
        )
    }
}
Write-Output "release dependencies ready in $stage"
