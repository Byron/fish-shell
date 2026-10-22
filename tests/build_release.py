#!/usr/bin/env python3
"""Check release publication with a fake Cargo, without compiling or replacing Fish."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


REPO = Path(__file__).resolve().parent.parent
PWSH = shutil.which("pwsh")
TARGET = "x86_64-pc-windows-msvc"
NAMES = ("fish.exe", "fish.pdb", "rash.exe", "rash.pdb")

DRIVER = r"""
param($Mode, $MockRoot, $ReleaseScript)
$ErrorActionPreference = 'Stop'
$global:LASTEXITCODE = 0
$targetRoot = Join-Path $MockRoot 'custom target'
$published = Join-Path $targetRoot 'x86_64-pc-windows-msvc/release'
function cargo {
    $global:LASTEXITCODE = 0
    if ($args[0] -eq 'metadata') {
        @{
            target_directory = $targetRoot
            build_directory = (Join-Path $MockRoot 'separate build')
            packages = @(@{
                id = 'mock-fish'; name = 'fish'
                manifest_path = (Join-Path (Split-Path (Split-Path $ReleaseScript)) 'Cargo.toml')
                targets = @(@{name='fish';kind=@('bin')}, @{name='rash';kind=@('bin')})
            })
        } | ConvertTo-Json -Depth 8 -Compress
        return
    }
    if ($args[0] -ne 'build') { throw 'Unexpected Cargo invocation' }
    $args | ConvertTo-Json | Set-Content (Join-Path $MockRoot 'build-args.json')
    foreach ($name in @('fish.exe', 'fish.pdb', 'rash.exe', 'rash.pdb')) {
        if ([IO.File]::ReadAllText((Join-Path $published $name)) -ne "old $name") {
            throw "Published artifact moved before build: $name"
        }
    }
    [IO.File]::WriteAllText((Join-Path $MockRoot 'old-intact-during-build'), 'yes')
    if ($Mode -eq 'build-failure') { $global:LASTEXITCODE = 17; return }
    $stage = $args[[array]::IndexOf($args, '--target-dir') + 1]
    $output = Join-Path $stage 'x86_64-pc-windows-msvc/release'
    if ($Mode -eq 'out-of-stage') { $output = Join-Path $MockRoot 'outside-stage' }
    [IO.Directory]::CreateDirectory($output) | Out-Null
    foreach ($name in @('fish', 'rash')) {
        if ($Mode -eq 'incomplete' -and $name -eq 'rash') { continue }
        $exe = Join-Path $output "$name.exe"
        $pdb = Join-Path $output "$name.pdb"
        [IO.File]::WriteAllText($exe, "new $name.exe")
        [IO.File]::WriteAllText($pdb, "new $name.pdb")
        $filenames = @($exe, $pdb)
        if ($Mode -eq 'no-symbols') { $filenames = @($exe) }
        @{
            reason = 'compiler-artifact'; package_id = 'mock-fish'
            target = @{name=$name;kind=@('bin')}; executable = $exe
            filenames = $filenames
        } | ConvertTo-Json -Depth 8 -Compress
    }
}
function Move-Item {
    param($LiteralPath, $Destination)
    if ($Mode -eq 'publish-failure' -and $LiteralPath -like '*.ready-*' -and
        [IO.Path]::GetFileName($LiteralPath) -eq 'rash.exe') {
        throw 'Injected failure after publishing Fish but before publishing Rash'
    }
    Microsoft.PowerShell.Management\Move-Item -LiteralPath $LiteralPath -Destination $Destination
}
$held = $null
try {
    if ($Mode -eq 'locked') {
        $held = [IO.File]::Open((Join-Path $published 'rash.exe'), 'Open', 'Read', 'Read')
    }
    if ($Mode -eq 'default-target') { & $ReleaseScript }
    else { & $ReleaseScript -Target 'x86_64-pc-windows-msvc' }
    exit $LASTEXITCODE
} finally {
    if ($held) { $held.Dispose() }
}
"""


@unittest.skipUnless(os.name == "nt" and PWSH, "requires Windows and PowerShell 7")
class ReleasePublication(unittest.TestCase):
    def setUp(self):
        (REPO / "target").mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="release-test-", dir=REPO / "target")
        self.root = Path(self.temporary.name)
        self.assertEqual(self.root.resolve().parent, (REPO / "target").resolve())
        self.addCleanup(self.temporary.cleanup)
        self.output = self.root / "custom target" / TARGET / "release"
        self.output.mkdir(parents=True)
        for name in NAMES:
            (self.output / name).write_text("old " + name, encoding="utf8")
        self.driver = self.root / "driver.ps1"
        self.driver.write_text(DRIVER, encoding="utf8")

    def run_build(self, mode):
        return subprocess.run(
            [PWSH, "-NoLogo", "-NoProfile", "-NonInteractive", "-File", str(self.driver),
             mode, str(self.root), str(REPO / "build_tools/build-release.ps1")],
            capture_output=True, text=True, encoding="utf8", timeout=20,
        )

    def assert_old_bundle(self):
        for name in NAMES:
            self.assertEqual((self.output / name).read_text(encoding="utf8"), "old " + name)
        self.assertFalse(list(self.output.glob("*.previous-*")))

    def test_success_builds_before_preserving_and_publishing_entire_bundle(self):
        result = self.run_build("success")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.root / "old-intact-during-build").is_file())
        for name in NAMES:
            self.assertEqual((self.output / name).read_text(encoding="utf8"), "new " + name)
            old = list(self.output.glob(Path(name).stem + ".previous-*" + Path(name).suffix))
            self.assertEqual(len(old), 1)
            self.assertEqual(old[0].read_text(encoding="utf8"), "old " + name)
        self.assertFalse(list(self.output.glob(".ready-*")))
        args = json.loads((self.root / "build-args.json").read_text(encoding="utf-8-sig"))
        self.assertEqual(args[args.index("--target") + 1], TARGET)
        self.assertEqual(Path(args[args.index("--target-dir") + 1]), self.root / "custom target/.release-build")
        config = args[args.index("--config") + 1]
        self.assertEqual(Path(json.loads(config.split("=", 1)[1])), self.root / "custom target/.release-build/build")

    def test_failed_build_leaves_published_bundle_untouched(self):
        result = self.run_build("build-failure")
        self.assertEqual(result.returncode, 17, result.stderr)
        self.assert_old_bundle()

    def test_incomplete_build_leaves_published_bundle_untouched(self):
        result = self.run_build("incomplete")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Cargo did not produce rash", result.stderr)
        self.assert_old_bundle()

    def test_failed_preservation_restores_already_moved_artifacts(self):
        result = self.run_build("locked")
        self.assertNotEqual(result.returncode, 0)
        self.assert_old_bundle()

    def test_failed_publication_restores_entire_previous_bundle(self):
        result = self.run_build("publish-failure")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Injected failure", result.stderr)
        self.assert_old_bundle()

    def test_out_of_stage_artifact_cannot_replace_published_bundle(self):
        result = self.run_build("out-of-stage")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("out-of-stage release artifact", result.stderr)
        self.assert_old_bundle()

    def test_stale_cached_symbols_are_not_published(self):
        result = self.run_build("no-symbols")
        self.assertEqual(result.returncode, 0, result.stderr)
        for name in ("fish.pdb", "rash.pdb"):
            self.assertFalse((self.output / name).exists())
            self.assertEqual(len(list(self.output.glob(Path(name).stem + ".previous-*.pdb"))), 1)

    def test_default_target_and_explicit_release_recipe(self):
        result = self.run_build("default-target")
        self.assertEqual(result.returncode, 0, result.stderr)
        args = json.loads((self.root / "build-args.json").read_text(encoding="utf-8-sig"))
        self.assertEqual(args[args.index("--target") + 1], TARGET)
        just = shutil.which("just")
        if just:
            # A copied recipe file prevents a regression from rebuilding the
            # actual checkout when testing a bare invocation.
            shutil.copy2(REPO / "justfile", self.root / "justfile")
            result = subprocess.run([just], cwd=self.root, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(b"Available recipes:", result.stdout)
            self.assertIn(b"release", result.stdout)
            result = subprocess.run([just, "--dry-run", "release"], cwd=self.root, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(b"build-release.ps1", result.stderr)


if __name__ == "__main__":
    unittest.main()
