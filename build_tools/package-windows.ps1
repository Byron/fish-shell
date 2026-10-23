# Build before assembling and publishing a portable native Windows archive.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Destination,
    [ValidatePattern('^[A-Za-z0-9_][A-Za-z0-9_.-]*$')]
    [string]$Target = 'x86_64-pc-windows-msvc'
)
$ErrorActionPreference = 'Stop'
if (!$Destination -or [IO.Path]::GetExtension($Destination) -ne '.zip') {
    throw 'Destination must explicitly name a .zip file.'
}
$archive = [IO.Path]::GetFullPath($Destination)
if (Test-Path -LiteralPath $archive -PathType Container) { throw 'Archive destination is a directory.' }
$parent = Split-Path -Parent $archive
[IO.Directory]::CreateDirectory($parent) | Out-Null
$archiveLock = [IO.File]::Open(($archive + '.lock'), 'OpenOrCreate', 'ReadWrite', 'None')
$projectRoot = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $projectRoot
try {
    & (Join-Path $PSScriptRoot 'build-release.ps1') -Target $Target
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $metadataJson = & cargo metadata --locked --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $metadata = ($metadataJson -join "`n") | ConvertFrom-Json
    $package = $metadata.packages | Where-Object { [IO.Path]::GetFullPath($_.manifest_path) -eq (Join-Path $projectRoot 'Cargo.toml') }
    if (!$package) { throw 'Cargo metadata did not identify the Fish package.' }
    $release = Join-Path $metadata.target_directory "$Target/release"
    $suffix = [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfffffffZ') + '-' + [guid]::NewGuid().ToString('N')
    $ready = Join-Path $parent ".fish-package-ready-$suffix"
    $readyZip = Join-Path $parent ".fish-package-ready-$suffix.zip"
    [IO.Directory]::CreateDirectory($ready) | Out-Null
    $inputs = [ordered]@{}
    foreach ($name in @('fish', 'rash', 'fish_indent', 'fish_key_reader')) {
        $inputs["$name.exe"] = Join-Path $release "$name.exe"
    }
    $inputs['COPYING'] = Join-Path $projectRoot 'COPYING'
    $inputs['license.rst'] = Join-Path $projectRoot 'doc_src/license.rst'
    $inputs['Cargo.lock'] = Join-Path $projectRoot 'Cargo.lock'
    $inputs['native-tools.md'] = Join-Path $projectRoot 'doc_internal/native-tools.md'
    $inputs['native-windows.md'] = Join-Path $projectRoot 'doc_internal/native-windows.md'
    $inputs['native-process-substitution.md'] = Join-Path $projectRoot 'doc_internal/native-process-substitution.md'
    $inputs['native-reader-customization.md'] = Join-Path $projectRoot 'doc_internal/native-reader-customization.md'
    $inputs['native-parity-tests.md'] = Join-Path $projectRoot 'doc_internal/native-parity-tests.md'
    $inputs['native-utilities-follow-ups.md'] = Join-Path $projectRoot 'doc_internal/native-utilities-follow-ups.md'
    $inputs['gix-follow-ups.md'] = Join-Path $projectRoot 'doc_internal/gix-follow-ups.md'
    $inputs['install.ps1'] = Join-Path $PSScriptRoot 'install-windows.ps1'
    foreach ($name in $inputs.Keys) {
        Copy-Item -LiteralPath $inputs[$name] -Destination (Join-Path $ready $name)
    }
    [IO.File]::WriteAllText((Join-Path $ready 'README.txt'), @'
Fish native Windows portable bundle

Run fish.exe in this directory, or install the extracted bundle:
  pwsh -File ./install.ps1 -Bundle . -Destination C:/your/chosen/Fish

The installer stages and checks all files before replacing a previous managed
bundle. Previous files are retained in a .previous-* directory for rollback.
Choose an empty destination or a directory managed by this installer. Neither
the installer nor these tools change PATH or your Fish configuration.

Functions, completions, and startup assets are embedded in fish.exe. Existing
configuration remains in its configured home (see native-windows.md).
fish_indent.exe and fish_key_reader.exe are described in native-tools.md.
Cargo.lock records the exact tested dependencies. See COPYING and license.rst
for the license, and the follow-up documents for platform/library limitations.
'@)
    $files = @(Get-ChildItem -LiteralPath $ready -File | Sort-Object Name | ForEach-Object {
        @{ name = $_.Name; sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
    })
    $manifest = @{ package = 'fish-native'; format = 1; version = $package.version; target = $Target; files = $files }
    [IO.File]::WriteAllText((Join-Path $ready 'manifest.json'), ($manifest | ConvertTo-Json -Depth 8))
    [IO.Compression.ZipFile]::CreateFromDirectory($ready, $readyZip)

    # The complete archive is ready before an earlier archive is moved aside.
    $backup = $archive + '.previous-' + $suffix
    $preserved = $false
    try {
        if (Test-Path -LiteralPath $archive -PathType Leaf) {
            Move-Item -LiteralPath $archive -Destination $backup
            $preserved = $true
        }
        Move-Item -LiteralPath $readyZip -Destination $archive
    } catch {
        $failure = $_
        if ($preserved) {
            try { Move-Item -LiteralPath $backup -Destination $archive }
            catch { Write-Warning "Previous archive remains at ${backup}: $_" }
        }
        throw $failure
    }
    # Only generated leaf files and their now-empty directory are removed.
    foreach ($file in $files) { Remove-Item -LiteralPath (Join-Path $ready $file.name) }
    Remove-Item -LiteralPath (Join-Path $ready 'manifest.json')
    Remove-Item -LiteralPath $ready
    if ($preserved) { Write-Host "Preserved previous archive at $backup" }
    Write-Host "Packaged native Fish at $archive"
} finally { Pop-Location; $archiveLock.Dispose() }
