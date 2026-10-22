# Preserve in-use Windows binaries before rebuilding the release used by Rustty.
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$target = 'x86_64-pc-windows-msvc'

Push-Location -LiteralPath $projectRoot
try {
    # Ask Cargo for its resolved paths, including CARGO_TARGET_DIR and an
    # independently configured build.build-dir on Cargo versions supporting it.
    $metadataJson = & cargo metadata --locked --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $metadata = ($metadataJson -join "`n") | ConvertFrom-Json
    $manifest = Join-Path $projectRoot 'Cargo.toml'
    $package = $metadata.packages | Where-Object {
        [IO.Path]::GetFullPath($_.manifest_path) -eq $manifest
    }
    if (!$package) { throw 'Cargo metadata did not identify the Fish package.' }
    $binaries = $package.targets | Where-Object { $_.kind -contains 'bin' }
    if (!$binaries) { throw 'Cargo metadata did not identify any Fish binaries.' }

    $targetRoot = [IO.Path]::GetFullPath($metadata.target_directory)
    $buildRoot = if ($metadata.build_directory) {
        [IO.Path]::GetFullPath($metadata.build_directory)
    } else { $targetRoot }
    $directories = @(
        (Join-Path $targetRoot "$target/release")
        (Join-Path $buildRoot "$target/release/deps")
    ) | Select-Object -Unique
    $suffix = [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfffffffZ') + '-' + [guid]::NewGuid().ToString('N')

    foreach ($directory in $directories) {
        foreach ($binary in $binaries) {
            foreach ($extension in @('.exe', '.pdb')) {
                $path = Join-Path $directory ($binary.name + $extension)
                if (Test-Path -LiteralPath $path -PathType Leaf) {
                    # Cargo's public executable and deps executable can be
                    # hardlinks to the same running image. Preserve both names.
                    # Exact names ensure earlier backups are never moved again.
                    $backup = $binary.name + '.previous-' + $suffix + $extension
                    Rename-Item -LiteralPath $path -NewName $backup
                    Write-Host "Preserved $path as $backup"
                }
            }
        }
    }

    & cargo build --locked --release --package fish --bins --target $target
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally {
    Pop-Location
}
