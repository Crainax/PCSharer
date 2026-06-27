$ErrorActionPreference = "Stop"

$packageJson = Get-Content -LiteralPath ".\package.json" -Raw | ConvertFrom-Json
$tauriConfig = Get-Content -LiteralPath ".\src-tauri\tauri.conf.json" -Raw | ConvertFrom-Json
$cargoToml = Get-Content -LiteralPath ".\src-tauri\Cargo.toml" -Raw

if ($cargoToml -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
  throw "Cannot read package.version from src-tauri/Cargo.toml"
}

$versions = [ordered]@{
  "package.json" = [string]$packageJson.version
  "src-tauri/tauri.conf.json" = [string]$tauriConfig.version
  "src-tauri/Cargo.toml" = [string]$Matches[1]
}

$uniqueVersions = @($versions.Values | Select-Object -Unique)

Write-Host "Version check:"
$versions.GetEnumerator() | ForEach-Object {
  Write-Host ("- {0}: {1}" -f $_.Key, $_.Value)
}

if ($uniqueVersions.Count -ne 1) {
  throw "Version mismatch. Keep package.json, tauri.conf.json and Cargo.toml package.version consistent."
}

Write-Host "Version check passed: $($uniqueVersions[0])" -ForegroundColor Green
