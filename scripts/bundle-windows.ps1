# Run in PowerShell on Windows with the Visual Studio C++ build tools installed.
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:OS -ne 'Windows_NT') {
    throw 'Build on Windows with the x64 MSVC toolchain and Windows SDK installed.'
}
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw 'Install stable Rust 1.92 or newer from https://rustup.rs (MSVC toolchain).'
    }
    # A real release build is required: GPUI compiles HLSL with fxc.exe here.
    & cargo build --locked --release --bin tree-lens --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Windows release build failed.' }

    $metadataJson = & cargo metadata --locked --format-version 1 --filter-platform x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Cannot collect dependency license metadata.' }
    $metadata = ($metadataJson -join "`n") | ConvertFrom-Json
    $exe = Join-Path $metadata.target_directory 'x86_64-pc-windows-msvc/release/tree-lens.exe'
    if (-not (Test-Path -LiteralPath $exe)) { throw "Missing release executable: $exe" }

    # Inspect the actual PE header rather than assuming the Rust attribute worked.
    $bytes = [System.IO.File]::ReadAllBytes($exe)
    $peOffset = [BitConverter]::ToInt32($bytes, 0x3c)
    $subsystem = [BitConverter]::ToUInt16($bytes, $peOffset + 24 + 68)
    if ($subsystem -ne 2) { throw 'Release executable must use the Windows GUI subsystem (no console).' }

    # Stage into a new directory; never recursively delete an existing user path.
    $stage = Join-Path ([System.IO.Path]::GetTempPath()) ('tree-lens-bundle-' + [guid]::NewGuid().ToString('N'))
    $bundle = Join-Path $stage 'Tree-Lens-Windows-x64'
    $licenses = Join-Path $bundle 'licenses'
    New-Item -ItemType Directory -Path $licenses -Force | Out-Null
    Copy-Item -LiteralPath $exe -Destination (Join-Path $bundle 'tree-lens.exe')
    Copy-Item -LiteralPath 'LICENSE' -Destination (Join-Path $bundle 'LICENSE.txt')
    Copy-Item -LiteralPath 'README.md' -Destination $bundle
    Copy-Item -LiteralPath 'docs/WINDOWS.md' -Destination $bundle

    $notices = [System.Collections.Generic.List[string]]::new()
    $notices.Add('# Dependency license inventory')
    $notices.Add('Generated from Cargo metadata for the Windows build. This conservative inventory may include build/test-only packages. License files retain their original package-relative paths.')
    foreach ($package in ($metadata.packages | Sort-Object name, version)) {
        $id = "$($package.name)-$($package.version)"
        $notices.Add("`n## $id`nLicense: $($package.license)`nSource: $($package.source)`nRepository: $($package.repository)")
        $packageRoot = Split-Path -Parent $package.manifest_path
        # Include nested asset/font licenses as well as each crate's main license.
        if ($package.name -eq 'tree-lens') { continue }
        $licenseFiles = @(Get-ChildItem -LiteralPath $packageRoot -File -Recurse | Where-Object {
            $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE|COPYRIGHT|OFL)([._-].*)?$'
        })
        if ($package.license_file) {
            $explicitLicense = Join-Path $packageRoot $package.license_file
            if (Test-Path -LiteralPath $explicitLicense) { $licenseFiles += Get-Item -LiteralPath $explicitLicense }
        }
        foreach ($file in ($licenseFiles | Sort-Object FullName -Unique)) {
            $relative = $file.FullName.Substring($packageRoot.Length).TrimStart([char[]]'\/')
            $destination = Join-Path (Join-Path $licenses $id) $relative
            New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null
            Copy-Item -LiteralPath $file.FullName -Destination $destination
        }
    }
    $notices | Set-Content -LiteralPath (Join-Path $bundle 'THIRD-PARTY-NOTICES.md') -Encoding UTF8
    New-Item -ItemType Directory -Path 'dist' -Force | Out-Null
    $archive = Join-Path $root 'dist/Tree-Lens-Windows-x64.zip'
    Compress-Archive -Path $bundle -DestinationPath $archive -Force
    Write-Host "Created: $archive"
    Write-Host "Staging files retained at: $stage"
    Write-Host 'Unsigned development build. Extract the ZIP before launching tree-lens.exe.'
} finally {
    Pop-Location
}
