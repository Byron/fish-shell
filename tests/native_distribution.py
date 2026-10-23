#!/usr/bin/env python3
"""Verify portable Windows packaging/installation using private mocked bundles."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import zipfile

REPO = Path(__file__).resolve().parent.parent
PWSH = shutil.which("pwsh")
JUST = shutil.which("just") or str(Path.home() / ".cargo/bin/just.exe")
NAMES = ("fish.exe", "rash.exe", "fish_indent.exe", "fish_key_reader.exe")

INSTALL_DRIVER = r'''
param($Root, $Script, $Mode)
$ErrorActionPreference = 'Stop'
$destination = Join-Path $Root 'installed'
$bundle = Join-Path $Root 'bundle'
function Move-Item {
    param($LiteralPath, $Destination)
    if ($Mode -eq 'publish-failure' -and $LiteralPath -like '*.fish-install-ready-*' -and
        [IO.Path]::GetFileName($LiteralPath) -eq 'rash.exe') { throw 'Injected publication failure' }
    Microsoft.PowerShell.Management\Move-Item -LiteralPath $LiteralPath -Destination $Destination
}
$held = $null
try {
    if ($Mode -eq 'locked') { $held = [IO.File]::Open((Join-Path $destination 'rash.exe'), 'Open', 'Read', 'Read') }
    & $Script -Bundle $bundle -Destination $destination
} finally { if ($held) { $held.Dispose() } }
'''

PACKAGE_DRIVER = r'''
param($Root, $Script, $Mode)
$ErrorActionPreference = 'Stop'
$global:LASTEXITCODE = 0
$targetRoot = Join-Path $Root 'build'
$archive = Join-Path $Root 'fish.zip'
function cargo {
    $global:LASTEXITCODE = 0
    if ($args[0] -eq 'metadata') {
        @{
            target_directory = $targetRoot
            packages = @(@{
                id='mock-fish';name='fish';version='4.9.3'
                manifest_path=(Join-Path (Split-Path (Split-Path $Script)) 'Cargo.toml')
                targets=@('fish','rash','fish_indent','fish_key_reader' | ForEach-Object { @{name=$_;kind=@('bin')} })
            })
        } | ConvertTo-Json -Depth 8 -Compress
        return
    }
    if ($args[0] -ne 'build') { throw 'Unexpected Cargo command' }
    if ([IO.File]::ReadAllText($archive) -ne 'old archive') { throw 'Archive moved before build completed' }
    if ($Mode -eq 'build-failure') { $global:LASTEXITCODE = 17; return }
    $stage = $args[[array]::IndexOf($args, '--target-dir') + 1]
    $output = Join-Path $stage 'x86_64-pc-windows-msvc/release'
    [IO.Directory]::CreateDirectory($output) | Out-Null
    foreach ($name in @('fish','rash','fish_indent','fish_key_reader')) {
        $exe = Join-Path $output "$name.exe"
        [IO.File]::WriteAllText($exe, "new $name")
        @{reason='compiler-artifact';package_id='mock-fish';target=@{name=$name;kind=@('bin')};executable=$exe;filenames=@($exe)} |
            ConvertTo-Json -Depth 8 -Compress
    }
}
function Move-Item {
    param($LiteralPath, $Destination)
    if ($Mode -eq 'publish-failure' -and $LiteralPath -like '*.fish-package-ready-*.zip' -and
        $Destination -eq $archive) { throw 'Injected archive publication failure' }
    Microsoft.PowerShell.Management\Move-Item -LiteralPath $LiteralPath -Destination $Destination
}
& $Script -Destination $archive
exit $LASTEXITCODE
'''


@unittest.skipUnless(os.name == "nt" and PWSH, "requires Windows and PowerShell 7")
class Distribution(unittest.TestCase):
    def setUp(self):
        (REPO / "target").mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="native-distribution-", dir=REPO / "target")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bundle = self.root / "bundle"
        self.installed = self.root / "installed"

    def make_bundle(self, path, version="new", extras=()):
        path.mkdir()
        files = []
        for name in (*NAMES, *extras):
            data = (version + " " + name).encode()
            (path / name).write_bytes(data)
            files.append(dict(name=name, sha256=hashlib.sha256(data).hexdigest()))
        manifest = dict(package="fish-native", format=1, target="x86_64-pc-windows-msvc", version="4.9.3", files=files)
        (path / "manifest.json").write_text(json.dumps(manifest), encoding="utf8")
        return manifest

    def powershell(self, *arguments):
        return subprocess.run([PWSH, "-NoLogo", "-NoProfile", "-NonInteractive", *map(str, arguments)],
                              capture_output=True, text=True, encoding="utf8", timeout=30)

    def install(self, mode="success"):
        driver = self.root / "install-driver.ps1"
        driver.write_text(INSTALL_DRIVER, encoding="utf8")
        return self.powershell("-File", driver, self.root, REPO / "build_tools/install-windows.ps1", mode)

    def assert_old(self):
        for name in NAMES:
            self.assertEqual((self.installed / name).read_text(), "old " + name)

    def test_install_new_and_upgrade_preserves_complete_old_bundle(self):
        self.make_bundle(self.bundle)
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.installed / "fish.exe").read_text(), "new fish.exe")
        self.assertFalse(list(self.installed.glob(".previous-*")))
        (self.bundle / "fish.exe").write_text("newer fish.exe")
        manifest = json.loads((self.bundle / "manifest.json").read_text())
        manifest["files"][0]["sha256"] = hashlib.sha256(b"newer fish.exe").hexdigest()
        (self.bundle / "manifest.json").write_text(json.dumps(manifest))
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        backup, = self.installed.glob(".previous-*")
        self.assertEqual((backup / "fish.exe").read_text(), "new fish.exe")
        self.assertTrue((backup / "manifest.json").is_file())
        self.assertEqual((self.installed / "fish.exe").read_text(), "newer fish.exe")

    def test_corruption_leaves_previous_files_in_place(self):
        self.make_bundle(self.bundle)
        self.make_bundle(self.installed, "old")
        (self.bundle / "fish.exe").write_text("corrupted")
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Checksum mismatch", result.stderr)
        self.assert_old()
        self.assertFalse(list(self.installed.glob(".previous-*")))

    def test_unmanaged_directory_is_preserved(self):
        self.make_bundle(self.bundle)
        self.installed.mkdir()
        (self.installed / "personal.txt").write_text("untouched")
        self.assertNotEqual(self.install().returncode, 0)
        self.assertEqual(list(p.name for p in self.installed.iterdir()), ["personal.txt"])

    def test_root_and_overlapping_destinations_are_rejected(self):
        self.make_bundle(self.bundle)
        for destination in [self.root.anchor, self.bundle, self.bundle / "nested", self.root]:
            result = self.powershell("-File", REPO / "build_tools/install-windows.ps1",
                                     "-Bundle", self.bundle, "-Destination", destination)
            self.assertNotEqual(result.returncode, 0, destination)
        self.assertFalse((self.bundle / "nested").exists())

    def test_invalid_manifest_paths_duplicates_and_device_names_are_rejected(self):
        manifest = self.make_bundle(self.bundle)
        for name in ["../outside", "nested/file", "C:/outside", "NUL.txt", "fish.exe", "trailing."]:
            mutated = dict(manifest, files=[*manifest["files"], dict(name=name, sha256="0" * 64)])
            (self.bundle / "manifest.json").write_text(json.dumps(mutated))
            self.assertNotEqual(self.install().returncode, 0, name)
            self.assertFalse(self.installed.exists(), name)

    def test_locked_previous_file_rolls_back_all_preserved_files(self):
        self.make_bundle(self.bundle)
        self.make_bundle(self.installed, "old")
        self.assertNotEqual(self.install("locked").returncode, 0)
        self.assert_old()

    def test_failed_publication_rolls_back_entire_previous_bundle(self):
        self.make_bundle(self.bundle)
        self.make_bundle(self.installed, "old")
        result = self.install("publish-failure")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Injected publication failure", result.stderr)
        self.assert_old()

    def test_obsolete_managed_files_move_to_backup(self):
        self.make_bundle(self.bundle)
        self.make_bundle(self.installed, "old", extras=("obsolete.txt",))
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.installed / "obsolete.txt").exists())
        backup, = self.installed.glob(".previous-*")
        self.assertEqual((backup / "obsolete.txt").read_text(), "old obsolete.txt")

    def package(self, mode):
        archive = self.root / "fish.zip"
        archive.write_text("old archive")
        driver = self.root / "package-driver.ps1"
        driver.write_text(PACKAGE_DRIVER, encoding="utf8")
        return self.powershell("-File", driver, self.root, REPO / "build_tools/package-windows.ps1", mode)

    def test_package_builds_before_publishing_and_records_all_checksums(self):
        result = self.package("success")
        self.assertEqual(result.returncode, 0, result.stderr)
        with zipfile.ZipFile(self.root / "fish.zip") as archive:
            manifest = json.loads(archive.read("manifest.json"))
            names = [entry["name"] for entry in manifest["files"]]
            self.assertTrue(set(NAMES).issubset(names))
            self.assertIn("install.ps1", names)
            for name in ("native-process-substitution.md", "native-reader-customization.md", "native-parity-tests.md"):
                self.assertIn(name, names)
            for entry in manifest["files"]:
                self.assertEqual(hashlib.sha256(archive.read(entry["name"])).hexdigest(), entry["sha256"])
        backup, = self.root.glob("fish.zip.previous-*")
        self.assertEqual(backup.read_text(), "old archive")

    def test_failed_build_and_failed_archive_publication_preserve_existing_archive(self):
        for mode in ["build-failure", "publish-failure"]:
            result = self.package(mode)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual((self.root / "fish.zip").read_text(), "old archive")

    def test_just_parameters_are_passed_as_data(self):
        scripts = self.root / "build_tools"
        scripts.mkdir()
        shutil.copyfile(REPO / "justfile", self.root / "justfile")
        (scripts / "package-windows.ps1").write_text(
            "param($Destination)\n[IO.File]::WriteAllText('received.json', (ConvertTo-Json $Destination))", encoding="utf8")
        value = "a' ; $unexpected `quoted.zip"
        result = subprocess.run([JUST, "package", value], cwd=self.root, capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads((self.root / "received.json").read_text()), value)
        result = subprocess.run([JUST], cwd=self.root, capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Available recipes", result.stdout)


if __name__ == "__main__":
    unittest.main()
