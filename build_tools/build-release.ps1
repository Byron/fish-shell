# Build away from published executables, then replace the complete release bundle.
[CmdletBinding()]
param(
    [ValidatePattern('^[A-Za-z0-9_][A-Za-z0-9_.-]*$')]
    [string]$Target
)

$ErrorActionPreference = 'Stop'
if (!$Target) { throw 'Specify a Rust target explicitly with -Target <target-triple>.' }
$projectRoot = Split-Path -Parent $PSScriptRoot

Push-Location -LiteralPath $projectRoot
try {
    $metadataJson = & cargo metadata --locked --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $metadata = ($metadataJson -join "`n") | ConvertFrom-Json
    $manifest = Join-Path $projectRoot 'Cargo.toml'
    $package = $metadata.packages | Where-Object {
        [IO.Path]::GetFullPath($_.manifest_path) -eq $manifest
    }
    if (!$package) { throw 'Cargo metadata did not identify the Fish package.' }
    $binaries = @($package.targets | Where-Object { $_.kind -contains 'bin' })
    if (!$binaries.Count) { throw 'Cargo metadata did not identify any Fish binaries.' }

    $targetRoot = [IO.Path]::GetFullPath($metadata.target_directory)
    $stageRoot = Join-Path $targetRoot '.release-build'
    $releaseDirectory = Join-Path $targetRoot "$Target/release"
    [IO.Directory]::CreateDirectory($targetRoot) | Out-Null
    # Serialize publication too: Cargo's own lock ends before the bundle is copied.
    $lockPath = Join-Path $targetRoot ".release-$Target.lock"
    $releaseLock = [IO.File]::Open($lockPath, 'OpenOrCreate', 'ReadWrite', 'None')
    try {
        $buildArgs = @('build', '--locked', '--release', '--package', 'fish', '--bins',
            '--target', $Target, '--target-dir', $stageRoot,
            '--message-format=json-render-diagnostics')
        if ($metadata.PSObject.Properties.Name -contains 'build_directory') {
            # A configured build.build-dir can otherwise point back at in-use
            # published deps, even when --target-dir selects a staging directory.
            $buildConfig = 'build.build-dir=' + (ConvertTo-Json -Compress (Join-Path $stageRoot 'build'))
            $buildArgs += @('--config', $buildConfig)
        }
        $artifacts = @{}
        & cargo @buildArgs | ForEach-Object {
            $message = $_ | ConvertFrom-Json
            if ($message.reason -eq 'compiler-artifact' -and
                $message.package_id -eq $package.id -and
                $message.target.kind -contains 'bin' -and $message.executable) {
                $artifacts[$message.target.name] = $message
            }
        }
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

        # Validate every binary before moving any part of the previous bundle.
        $files = @{}
        $stagePrefix = [IO.Path]::GetFullPath($stageRoot).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
        foreach ($binary in $binaries) {
            $artifact = $artifacts[$binary.name]
            if (!$artifact) { throw "Cargo did not produce $($binary.name)." }
            # Only publish artifacts reported by this build; a cached PDB can
            # remain on disk after a later build disables debug information.
            $paths = @(@($artifact.executable) + @($artifact.filenames) | Sort-Object -Unique)
            foreach ($path in $paths) {
                $path = [IO.Path]::GetFullPath($path)
                if (!$path.StartsWith($stagePrefix, [StringComparison]::OrdinalIgnoreCase) -or
                    !(Test-Path -LiteralPath $path -PathType Leaf)) {
                    throw "Missing or out-of-stage release artifact: $path"
                }
                $name = [IO.Path]::GetFileName($path)
                if ($files.ContainsKey($name)) { throw "Duplicate release artifact: $name" }
                $files[$name] = $path
            }
        }

        $suffix = [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfffffffZ') + '-' + [guid]::NewGuid().ToString('N')
        $readyDirectory = Join-Path $releaseDirectory ".ready-$suffix"
        [IO.Directory]::CreateDirectory($readyDirectory) | Out-Null
        foreach ($name in $files.Keys) {
            Copy-Item -LiteralPath $files[$name] -Destination (Join-Path $readyDirectory $name)
        }

        $backups = [Collections.Generic.List[object]]::new()
        $published = [Collections.Generic.List[string]]::new()
        try {
            # Preserve stale symbol files as well when a new build omits symbols.
            $oldNames = @($files.Keys) + @($binaries | ForEach-Object { $_.name + '.pdb' })
            foreach ($name in $oldNames) {
                if (Test-Path -LiteralPath (Join-Path $releaseDirectory $name) -PathType Container) {
                    throw "A directory occupies the release artifact path: $name"
                }
            }
            foreach ($name in ($oldNames | Sort-Object -Unique)) {
                $path = Join-Path $releaseDirectory $name
                if (Test-Path -LiteralPath $path -PathType Leaf) {
                    $backupName = [IO.Path]::GetFileNameWithoutExtension($name) + '.previous-' + $suffix + [IO.Path]::GetExtension($name)
                    $backup = Join-Path $releaseDirectory $backupName
                    Move-Item -LiteralPath $path -Destination $backup
                    $backups.Add(@{ Original = $path; Backup = $backup })
                }
            }
            foreach ($name in ($files.Keys | Sort-Object)) {
                $destination = Join-Path $releaseDirectory $name
                Move-Item -LiteralPath (Join-Path $readyDirectory $name) -Destination $destination
                $published.Add($destination)
            }
        } catch {
            # Keep the prepared bundle for inspection if publication fails.
            $publishError = $_
            foreach ($path in $published) {
                try {
                    Move-Item -LiteralPath $path -Destination (Join-Path $readyDirectory ([IO.Path]::GetFileName($path)))
                } catch { Write-Warning "Could not withdraw newly published artifact ${path}: $_" }
            }
            foreach ($backup in $backups) {
                try {
                    Move-Item -LiteralPath $backup.Backup -Destination $backup.Original
                } catch { Write-Warning "Previous artifact remains at $($backup.Backup): $_" }
            }
            throw $publishError
        }
        # All files were moved individually; this only removes the empty directory.
        Remove-Item -LiteralPath $readyDirectory
        foreach ($backup in $backups) { Write-Host "Preserved $($backup.Original) as $($backup.Backup)" }
        Write-Host "Published $Target release to $releaseDirectory"
    } finally {
        $releaseLock.Dispose()
    }
} finally {
    Pop-Location
}
