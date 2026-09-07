# Sign the built Cortex Windows installer(s) with a code-signing cert from the
# CurrentUser store, by thumbprint. SHA-256 digest + RFC-3161 timestamp (so the
# signature stays valid after the cert expires).
#
#   pwsh -File scripts/sign-windows.ps1 -Thumbprint <THUMBPRINT>
#
# Works with a self-signed cert (from make-selfsigned-cert.ps1) OR a real cert
# (Azure Trusted Signing / OV / EV) imported into CurrentUser\My — same command.
# Signs the NSIS per-user installer (no-admin) and the MSI if present.

param(
  [Parameter(Mandatory = $true)][string]$Thumbprint,
  [string]$TimestampUrl = "http://timestamp.digicert.com"
)
$ErrorActionPreference = "Stop"

# Locate signtool.exe from the installed Windows SDK.
$signtool = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin\*\x64\signtool.exe" -ErrorAction SilentlyContinue |
  Sort-Object FullName -Descending | Select-Object -First 1
if (-not $signtool) {
  throw "signtool.exe not found. Install the Windows 10/11 SDK (or run from a Developer PowerShell)."
}

$bundle = "src-tauri/target/release/bundle"
$targets = @()
$targets += Get-ChildItem "$bundle/nsis/*-setup.exe" -ErrorAction SilentlyContinue
$targets += Get-ChildItem "$bundle/msi/*.msi"        -ErrorAction SilentlyContinue
if ($targets.Count -eq 0) {
  throw "No installer found under $bundle. Build first: pnpm tauri build --bundles nsis"
}

foreach ($t in $targets) {
  Write-Host "Signing $($t.Name) ..." -ForegroundColor Cyan
  & $signtool.FullName sign /sha1 $Thumbprint /fd sha256 /tr $TimestampUrl /td sha256 /v $t.FullName
  if ($LASTEXITCODE -ne 0) { throw "signtool sign failed for $($t.Name)" }
  & $signtool.FullName verify /pa /v $t.FullName
  if ($LASTEXITCODE -ne 0) { throw "signtool verify failed for $($t.Name)" }
  Write-Host "  signed + verified: $($t.FullName)" -ForegroundColor Green
}

Write-Host ""
Write-Host "Distribute the NSIS *-setup.exe (per-user install, NO admin prompt)." -ForegroundColor Green
Write-Host "The MSI always requires admin — prefer the -setup.exe for no-admin installs."
