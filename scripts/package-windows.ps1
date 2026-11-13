# Build and package Shortlist as a portable Windows folder + release zip.
#
# Asset name contract (crates/update/src/lib.rs):
#   Shortlist-windows-x86_64.zip
#   Shortlist-windows-aarch64.zip
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$versionLine = Select-String -Path "Cargo.toml" -Pattern '^version = "(.*)"' | Select-Object -First 1
if (-not $versionLine) {
    throw "Could not read the workspace version from Cargo.toml."
}
$version = $versionLine.Matches[0].Groups[1].Value

$arch = switch ($env:PROCESSOR_ARCHITECTURE) {
    "AMD64" { "x86_64" }
    "ARM64" { "aarch64" }
    default { throw "Unsupported Windows architecture: $env:PROCESSOR_ARCHITECTURE" }
}

$modelDir = Join-Path $root "assets/models/e5-small-v2"
if (-not (Test-Path -LiteralPath (Join-Path $modelDir "model_quantized.onnx"))) {
    Write-Host "e5-small-v2 is not present; fetching it for the app bundle."
    & (Join-Path $PSScriptRoot "fetch-model.ps1")
}

Write-Host "Building Shortlist $version for Windows $arch..."
cargo build --release -p shortlist-app --bin shortlist
if ($LASTEXITCODE -ne 0) { throw "cargo build failed for shortlist" }
cargo build --release -p shortlist-parse --bin shortlist-pdf
if ($LASTEXITCODE -ne 0) { throw "cargo build failed for shortlist-pdf" }

$out = Join-Path $root "dist"
$app = Join-Path $out "Shortlist"
$zip = Join-Path $out "Shortlist-windows-$arch.zip"

if (Test-Path -LiteralPath $app) { Remove-Item -LiteralPath $app -Recurse -Force }
New-Item -ItemType Directory -Path (Join-Path $app "assets/models") -Force | Out-Null

Copy-Item -LiteralPath "target/release/shortlist.exe" -Destination (Join-Path $app "shortlist.exe")
Copy-Item -LiteralPath "target/release/shortlist-pdf.exe" -Destination (Join-Path $app "shortlist-pdf.exe")
Copy-Item -LiteralPath $modelDir -Destination (Join-Path $app "assets/models") -Recurse

$manifest = [ordered]@{
    app        = "shortlist"
    version    = $version
    platform   = "windows"
    executable = "shortlist.exe"
}
Set-Content -LiteralPath (Join-Path $app "shortlist-install.json") `
    -Value ($manifest | ConvertTo-Json) -Encoding ascii

foreach ($path in @($zip, "$zip.sha256")) {
    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force }
}

Write-Host "Packaging $zip..."
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
[System.IO.Compression.ZipFile]::CreateFromDirectory(
    $app,
    $zip,
    [System.IO.Compression.CompressionLevel]::Optimal,
    $true
)

$hash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
$name = Split-Path -Leaf $zip
Set-Content -LiteralPath "$zip.sha256" -Value "$hash  $name" -Encoding ascii

Write-Host ""
Write-Host "Built $zip"
Write-Host "Checksum: $hash"
