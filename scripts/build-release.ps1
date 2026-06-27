$ErrorActionPreference = "Stop"

function Invoke-NativeStep($FilePath, [string[]]$Arguments) {
  & $FilePath @Arguments

  if ($LASTEXITCODE -ne 0) {
    throw "Command failed: $FilePath $($Arguments -join ' ')"
  }
}

$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if ((Test-Path -LiteralPath $cargoBin) -and ($env:Path -notlike "*$cargoBin*")) {
  $env:Path = "$cargoBin;$env:Path"
}

if (-not (Test-Path -LiteralPath ".\node_modules")) {
  Invoke-NativeStep "npm" @("install")
}

Invoke-NativeStep "powershell" @("-ExecutionPolicy", "Bypass", "-File", ".\scripts\verify-version.ps1")
Invoke-NativeStep "npm" @("run", "check")
Invoke-NativeStep "npm" @("run", "tauri", "build", "--", "--no-bundle")

Write-Host ""
Write-Host "Release artifact:" -ForegroundColor Green
Write-Host "- .\src-tauri\target\release\pc-sharer.exe"
