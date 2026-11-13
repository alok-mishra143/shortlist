# Fetch the e5-small-v2 encoder used by Shortlist.
# The shell version lives in scripts/fetch-model.sh; this one keeps Windows
# packaging self-contained.
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$dir = Join-Path $root "assets/models/e5-small-v2"
New-Item -ItemType Directory -Path $dir -Force | Out-Null

$base = "https://huggingface.co/Xenova/e5-small-v2/resolve/main"
$files = @(
    @{ Remote = "onnx/model_quantized.onnx"; Name = "model_quantized.onnx" },
    @{ Remote = "tokenizer.json"; Name = "tokenizer.json" },
    @{ Remote = "tokenizer_config.json"; Name = "tokenizer_config.json" },
    @{ Remote = "special_tokens_map.json"; Name = "special_tokens_map.json" },
    @{ Remote = "config.json"; Name = "config.json" }
)

foreach ($file in $files) {
    $out = Join-Path $dir $file.Name
    if (Test-Path -LiteralPath $out) {
        Write-Host "  ok  $($file.Name)"
        continue
    }
    Write-Host "  get $($file.Name)"
    Invoke-WebRequest -Uri "$base/$($file.Remote)" -OutFile $out -UseBasicParsing
}

Write-Host "e5-small-v2 ready: $dir"
