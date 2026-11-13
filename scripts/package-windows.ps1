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

# First-install format: a per-user setup.exe with a Start Menu shortcut and an
# uninstaller. The zip above stays the in-app update format; the updater
# downloads, verifies, and extracts it.
$setupName = "Shortlist-windows-$arch-setup"
$setup = Join-Path $out "$setupName.exe"

$isccCandidates = @()
if (${env:ProgramFiles(x86)}) {
    $isccCandidates += (Join-Path ${env:ProgramFiles(x86)} "Inno Setup 6\ISCC.exe")
}
if ($env:ProgramFiles) {
    $isccCandidates += (Join-Path $env:ProgramFiles "Inno Setup 6\ISCC.exe")
}
$iscc = $null
foreach ($candidate in $isccCandidates) {
    if (Test-Path -LiteralPath $candidate) { $iscc = $candidate; break }
}
if (-not $iscc) {
    $command = Get-Command iscc.exe -ErrorAction SilentlyContinue
    if ($command) { $iscc = $command.Source }
}

if (-not $iscc) {
    Write-Host "Inno Setup (ISCC.exe) not found; skipping $setupName.exe."
} else {
    $iss = Join-Path $out "shortlist-setup.iss"
    $script = @"
#define AppName "Shortlist"
#define AppVersion "$version"
#define AppExe "shortlist.exe"

[Setup]
AppId={{6F0D9C7E-3B0A-4B7E-9A6E-2F4B1C8D5A31}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Shortlist
DefaultDirName={localappdata}\Programs\Shortlist
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\{#AppExe}

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional icons:"; Flags: unchecked

[Files]
Source: "$app\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\Shortlist"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\Shortlist"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "Launch Shortlist"; Flags: nowait postinstall skipifsilent
"@
    Set-Content -LiteralPath $iss -Value $script -Encoding ascii

    foreach ($path in @($setup, "$setup.sha256")) {
        if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force }
    }

    Write-Host "Packaging $setup..."
    & $iscc /Qp "/O$out" "/F$setupName" $iss
    if ($LASTEXITCODE -ne 0) { throw "Inno Setup failed with exit code $LASTEXITCODE" }
    Remove-Item -LiteralPath $iss -Force

    $setupHash = (Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash.ToLowerInvariant()
    $setupFile = Split-Path -Leaf $setup
    Set-Content -LiteralPath "$setup.sha256" -Value "$setupHash  $setupFile" -Encoding ascii

    Write-Host "Built $setup"
    Write-Host "Checksum: $setupHash"
}

Write-Host ""
Write-Host "Built $zip"
Write-Host "Checksum: $hash"
