# Install an extracted portable bundle to an explicit directory without changing PATH.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Bundle,
    [Parameter(Mandatory)][string]$Destination
)
$ErrorActionPreference = 'Stop'

function Read-BundleManifest([string]$Path) {
    $text = [IO.File]::ReadAllText($Path)
    $manifest = $text | ConvertFrom-Json
    if ($manifest.package -ne 'fish-native' -or $manifest.format -ne 1 -or
        !$manifest.files -or !$manifest.target) { throw "Invalid Fish bundle manifest: $Path" }
    $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($file in $manifest.files) {
        $name = [string]$file.name
        if ($name -notmatch '^[A-Za-z0-9][A-Za-z0-9_.-]*[A-Za-z0-9]$' -or
            $name -match '^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(\.|$)' -or
            $name -eq 'manifest.json' -or !$names.Add($name) -or
            [string]$file.sha256 -notmatch '^[a-fA-F0-9]{64}$') {
            throw "Invalid or duplicate bundle entry: $name"
        }
    }
    foreach ($required in @('fish.exe', 'rash.exe', 'fish_indent.exe', 'fish_key_reader.exe')) {
        if (!$names.Contains($required)) { throw "Bundle does not contain $required" }
    }
    return @{ Manifest = $manifest; Text = $text }
}

if (!$Bundle -or !$Destination) { throw 'Bundle and Destination cannot be empty.' }
$source = [IO.Path]::GetFullPath($Bundle).TrimEnd('\', '/')
$destinationPath = [IO.Path]::GetFullPath($Destination).TrimEnd('\', '/')
if (!$destinationPath -or $destinationPath -eq [IO.Path]::GetPathRoot($destinationPath).TrimEnd('\', '/')) {
    throw 'The installation destination cannot be a filesystem root.'
}
$separator = [IO.Path]::DirectorySeparatorChar
if ($source -eq $destinationPath -or
    $source.StartsWith($destinationPath + $separator, [StringComparison]::OrdinalIgnoreCase) -or
    $destinationPath.StartsWith($source + $separator, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Bundle and installation directories cannot contain each other.'
}
if (!(Test-Path -LiteralPath $source -PathType Container)) { throw "Bundle directory not found: $source" }
$parent = Split-Path -Parent $destinationPath
[IO.Directory]::CreateDirectory($parent) | Out-Null
$lockPath = $destinationPath + '.install.lock'
$installLock = [IO.File]::Open($lockPath, 'OpenOrCreate', 'ReadWrite', 'None')
try {
    $bundleData = Read-BundleManifest (Join-Path $source 'manifest.json')
    $files = @($bundleData.Manifest.files)
    $previousFiles = @()
    if (Test-Path -LiteralPath $destinationPath) {
        $item = Get-Item -LiteralPath $destinationPath -Force
        if (!$item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'The installation destination must be an ordinary directory.'
        }
        if (Get-ChildItem -LiteralPath $destinationPath -Force | Select-Object -First 1) {
            $previous = Read-BundleManifest (Join-Path $destinationPath 'manifest.json')
            $previousFiles = @($previous.Manifest.files)
        }
    }

    # Validate and copy every new file before moving any installed file.
    $suffix = [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfffffffZ') + '-' + [guid]::NewGuid().ToString('N')
    $ready = Join-Path $parent ".fish-install-ready-$suffix"
    $backup = Join-Path $destinationPath ".previous-$suffix"
    [IO.Directory]::CreateDirectory($ready) | Out-Null
    foreach ($file in $files) {
        $inputPath = Join-Path $source $file.name
        $item = Get-Item -LiteralPath $inputPath -Force
        if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "Bundle entry must be an ordinary file: $($file.name)"
        }
        $copy = Join-Path $ready $file.name
        Copy-Item -LiteralPath $inputPath -Destination $copy
        if ((Get-FileHash -LiteralPath $copy -Algorithm SHA256).Hash -ne $file.sha256) {
            throw "Checksum mismatch: $($file.name). Existing installation is unchanged; prepared files remain at $ready"
        }
    }
    [IO.File]::WriteAllText((Join-Path $ready 'manifest.json'), $bundleData.Text)
    [IO.Directory]::CreateDirectory($destinationPath) | Out-Null
    $newNames = @($files | ForEach-Object { $_.name }) + @('manifest.json')
    $allNames = @($newNames) + @($previousFiles | ForEach-Object { $_.name })
    $allNames = @($allNames | Sort-Object -Unique)
    foreach ($name in $allNames) {
        if (Test-Path -LiteralPath (Join-Path $destinationPath $name) -PathType Container) {
            throw "A directory occupies an installed file path: $name"
        }
    }
    $saved = [Collections.Generic.List[string]]::new()
    $published = [Collections.Generic.List[string]]::new()
    try {
        foreach ($name in $allNames) {
            $path = Join-Path $destinationPath $name
            if (Test-Path -LiteralPath $path -PathType Leaf) {
                [IO.Directory]::CreateDirectory($backup) | Out-Null
                Move-Item -LiteralPath $path -Destination (Join-Path $backup $name)
                $saved.Add($name)
            }
        }
        foreach ($name in $newNames) {
            Move-Item -LiteralPath (Join-Path $ready $name) -Destination (Join-Path $destinationPath $name)
            $published.Add($name)
        }
    } catch {
        $failure = $_
        foreach ($name in $published) {
            try { Move-Item -LiteralPath (Join-Path $destinationPath $name) -Destination (Join-Path $ready $name) }
            catch { Write-Warning "Could not withdraw new file ${name}: $_" }
        }
        foreach ($name in $saved) {
            try { Move-Item -LiteralPath (Join-Path $backup $name) -Destination (Join-Path $destinationPath $name) }
            catch { Write-Warning "Previous file remains at $(Join-Path $backup $name): $_" }
        }
        throw $failure
    }
    Remove-Item -LiteralPath $ready # Empty: every prepared file was moved individually.
    if ($saved.Count) { Write-Host "Preserved previous installation at $backup" }
    Write-Host "Installed Fish to $destinationPath. Add this directory to PATH to use these commands."

} finally { $installLock.Dispose() }
