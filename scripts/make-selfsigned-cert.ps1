# Create a self-signed CODE-SIGNING certificate in the current user's store.
#
# Run ONCE on the Windows build box. No administrator needed, no cost. Prints
# the thumbprint to hand to scripts/sign-windows.ps1.
#
#   pwsh -File scripts/make-selfsigned-cert.ps1
#
# Self-signed = the installer is signed and installs per-user with no admin, but
# Windows SmartScreen will still warn on first run (click "More info -> Run
# anyway"). For ZERO warnings, provision a real cert — see docs/WINDOWS-BUILD.md
# (Azure Trusted Signing, ~$10/mo) — and feed its thumbprint to sign-windows.ps1
# instead. The signing step is identical either way.

param(
  [string]$Subject = "CN=Cortex",
  [int]$Years = 5
)
$ErrorActionPreference = "Stop"

$cert = New-SelfSignedCertificate `
  -Type CodeSigningCert `
  -Subject $Subject `
  -KeyUsage DigitalSignature `
  -KeyAlgorithm RSA -KeyLength 3072 `
  -HashAlgorithm SHA256 `
  -CertStoreLocation "Cert:\CurrentUser\My" `
  -NotAfter (Get-Date).AddYears($Years)

Write-Host ""
Write-Host "Created code-signing certificate:" -ForegroundColor Green
Write-Host "  Subject:    $($cert.Subject)"
Write-Host "  Thumbprint: $($cert.Thumbprint)"
Write-Host "  Expires:    $($cert.NotAfter)"
Write-Host ""
Write-Host "Next: build, then sign with this thumbprint:" -ForegroundColor Cyan
Write-Host "  pnpm tauri build --bundles nsis"
Write-Host "  pwsh -File scripts/sign-windows.ps1 -Thumbprint $($cert.Thumbprint)"
Write-Host ""
Write-Host "Tip: keep this thumbprint; the cert lives in your CurrentUser\My store."
