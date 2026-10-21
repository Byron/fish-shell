#!/usr/bin/env python3
"""Offline checks for the reference-source integrity boundary."""

import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import fetch


class DownloadTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.cache = Path(self.temporary.name)
        self.contents = b"pinned source"
        self.digest = hashlib.sha256(self.contents).hexdigest()

    def test_download_is_verified_and_reused(self):
        with patch("fetch.urllib.request.urlopen", return_value=io.BytesIO(self.contents)) as request:
            result = fetch.fetch(self.cache, "source", "https://example.invalid/source", self.digest)
            self.assertEqual(result.read_bytes(), self.contents)
            self.assertEqual(fetch.fetch(self.cache, "source", "unused", self.digest), result)
            request.assert_called_once()

    def test_bad_download_is_not_cached(self):
        with patch("fetch.urllib.request.urlopen", return_value=io.BytesIO(b"changed source")):
            with self.assertRaisesRegex(ValueError, "SHA256 mismatch"):
                fetch.fetch(self.cache, "source", "https://example.invalid/source", self.digest)
        self.assertFalse((self.cache / "source").exists())

    def test_changed_cache_is_refused_without_overwriting(self):
        source = self.cache / "source"
        source.write_bytes(b"local changes")
        with patch("fetch.urllib.request.urlopen") as request:
            with self.assertRaisesRegex(ValueError, "SHA256 mismatch"):
                fetch.fetch(self.cache, "source", "unused", self.digest)
            request.assert_not_called()
        self.assertEqual(source.read_bytes(), b"local changes")

    def test_unrecognized_source_directory_is_preserved(self):
        source = self.cache / "bash-3.2"
        source.mkdir()
        local = source / "local-work"
        local.write_text("keep me")
        with patch("fetch.fetch", return_value=self.cache / "unused"):
            with self.assertRaisesRegex(ValueError, "Unrecognized source directory"):
                fetch.prepare(self.cache)
        self.assertEqual(local.read_text(), "keep me")

    def test_new_source_metadata_reuses_cache_and_checks_new_hash(self):
        archive = io.BytesIO()
        with tarfile.open(fileobj=archive, mode="w:gz") as tar:
            member = tarfile.TarInfo("bash-3.2/newly-ported.c")
            member.size = len(self.contents)
            tar.addfile(member, io.BytesIO(self.contents))
        data = archive.getvalue()
        manifest = {
            "archive": {"name": "source.tar.gz", "url": "https://example.invalid/source",
                        "sha256": hashlib.sha256(data).hexdigest()},
            "patch_base_url": "https://example.invalid/patches/", "patches": {},
            "ported_source_sha256": {},
        }
        manifest_path = self.cache / "sources.json"
        manifest_path.write_text(json.dumps(manifest))
        directory = self.cache / "oracle"
        with patch("fetch.__file__", str(self.cache / "fetch.py")):
            with patch("fetch.urllib.request.urlopen", return_value=io.BytesIO(data)):
                source = fetch.prepare(directory)
            marker = (source / ".rash-source").read_bytes()
            build_cache = directory / "build"
            build_cache.mkdir()
            (build_cache / "compiled-object").write_bytes(b"keep cached build")
            manifest["ported_source_sha256"]["newly-ported.c"] = self.digest
            manifest["signature_key_fingerprint"] = "new provenance metadata"
            manifest_path.write_text(json.dumps(manifest, indent=2))
            with patch("fetch.tarfile.open") as extract:
                self.assertEqual(fetch.prepare(directory), source)
                extract.assert_not_called()
            self.assertEqual((source / ".rash-source").read_bytes(), marker)
            self.assertEqual((build_cache / "compiled-object").read_bytes(), b"keep cached build")
            (source / "newly-ported.c").write_bytes(b"altered after metadata addition")
            with self.assertRaisesRegex(ValueError, "SHA256 mismatch"):
                fetch.prepare(directory)
            manifest["archive"]["url"] += "?changed-source"
            manifest_path.write_text(json.dumps(manifest))
            with self.assertRaisesRegex(ValueError, "Unrecognized source directory"):
                fetch.prepare(directory)


if __name__ == "__main__":
    unittest.main()
