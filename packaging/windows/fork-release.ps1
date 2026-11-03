# Publishes a release of this fork (gstar56/spotifast) from a Windows machine.
#
#   .\packaging\windows\fork-release.ps1 v0.12.1
#
# Upstream's release workflow signs the macOS app and the update checksums
# with keys this fork doesn't have, so the fork releases from here instead.
# It builds the Windows x64 installer with MilkDrop, names it the way the
# updater looks for it, writes checksums.txt and publishes both as the
# latest release. Installed copies then offer the update: they read this
# fork's latest release and verify the installer against checksums.txt.
#
# Run it from a Visual Studio x64 developer prompt with MilkDrop's build
# tools (CMake, Ninja, LLVM, and vcpkg's static GLEW in VCPKG_ROOT), after
# raising the version in Cargo.toml, adding
# packaging/release-notes/<tag>.md, and pushing that commit.

param(
    [Parameter(Mandatory)][string]$Tag,
    [string]$Repository = "gstar56/spotifast"
)
$ErrorActionPreference = "Stop"
Set-Location (Resolve-Path "$PSScriptRoot\..\..")

if ($Tag -notmatch '^v(\d+\.\d+\.\d+)$') { throw "Expected a tag like v0.12.1" }
$version = $Matches[1]
$cargoVersion = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
if ($cargoVersion -ne $version) { throw "Cargo.toml says $cargoVersion, not $version" }
$notes = "packaging\release-notes\$Tag.md"
if (-not (Test-Path $notes)) { throw "Write $notes first" }
if (git status --porcelain) { throw "Commit or stash your changes first" }
$commit = git rev-parse HEAD
git fetch --quiet origin
if (-not ((git branch -r --contains $commit) -match '^\s*origin/')) { throw "Push $commit first" }

# The machine may crash the compiler at random; a retry usually gets past it.
for ($attempt = 1; $true; $attempt++) {
    cargo build --release --locked --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -eq 0) { break }
    if ($attempt -ge 4) { throw "The release build failed" }
    Write-Warning "Build attempt $attempt failed, retrying"
}
$binary = "target\x86_64-pc-windows-msvc\release\spotifast.exe"
if (dumpbin /dependents $binary | Select-String -Pattern 'vcruntime|msvcp') {
    throw "The binary needs the Visual C++ runtime; it must link it statically"
}

$staging = "target\fork-release\$Tag"
Remove-Item -Recurse -Force $staging -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $staging | Out-Null
$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "Inno Setup 6 is not installed" }
& $iscc /Q "/DVersion=$version" "/DArch=x86_64" "/DBinary=$((Resolve-Path $binary).Path)" "/DOutputDir=$((Resolve-Path $staging).Path)" packaging\windows\spotifast.iss
if ($LASTEXITCODE -ne 0) { throw "The installer failed to build" }

python packaging\release-names.py $staging $Tag
if ($LASTEXITCODE -ne 0) { throw "Writing checksums.txt failed" }
Get-Content "$staging\checksums.txt"

gh release create $Tag --repo $Repository --target $commit --title "Spotifast $Tag" --notes-file $notes (Get-ChildItem $staging).FullName
if ($LASTEXITCODE -ne 0) { throw "Publishing the release failed" }
Write-Host "Published https://github.com/$Repository/releases/tag/$Tag"
