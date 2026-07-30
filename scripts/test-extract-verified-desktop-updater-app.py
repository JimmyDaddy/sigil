#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("extract-verified-desktop-updater-app.py")
SPEC = importlib.util.spec_from_file_location("desktop_updater_extract", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def write_archive(path: Path, members: list[tarfile.TarInfo]) -> None:
    with tarfile.open(path, "w:gz") as archive:
        for member in members:
            body = b"binary" if member.isfile() else None
            if body is not None:
                member.size = len(body)
            archive.addfile(member, io.BytesIO(body) if body is not None else None)


class ExtractVerifiedDesktopUpdaterAppTests(unittest.TestCase):
    def test_extracts_safe_app_archive(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "safe.tar.gz"
            app = tarfile.TarInfo("Sigil.app")
            app.type = tarfile.DIRTYPE
            contents = tarfile.TarInfo("Sigil.app/Contents")
            contents.type = tarfile.DIRTYPE
            binary = tarfile.TarInfo("Sigil.app/Contents/sigil-runtime")
            write_archive(archive, [app, contents, binary])
            extracted = MODULE.extract_verified_archive(archive, root / "out")
            self.assertEqual(extracted, root / "out" / "Sigil.app")
            self.assertEqual(
                (extracted / "Contents" / "sigil-runtime").read_bytes(),
                b"binary",
            )

    def test_rejects_member_path_traversal(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "traversal.tar.gz"
            member = tarfile.TarInfo("Sigil.app/../../escape")
            write_archive(archive, [member])
            with self.assertRaisesRegex(ValueError, "unsafe updater archive member path"):
                MODULE.extract_verified_archive(archive, root / "out")

    def test_rejects_absolute_member_path(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "absolute.tar.gz"
            member = tarfile.TarInfo("/Sigil.app/Contents/file")
            write_archive(archive, [member])
            with self.assertRaisesRegex(ValueError, "unsafe updater archive member path"):
                MODULE.extract_verified_archive(archive, root / "out")

    def test_rejects_escaping_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "symlink.tar.gz"
            app = tarfile.TarInfo("Sigil.app")
            app.type = tarfile.DIRTYPE
            link = tarfile.TarInfo("Sigil.app/Contents/link")
            link.type = tarfile.SYMTYPE
            link.linkname = "../../../escape"
            write_archive(archive, [app, link])
            with self.assertRaisesRegex(ValueError, "link target escapes"):
                MODULE.extract_verified_archive(archive, root / "out")

    def test_rejects_escaping_hardlink(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "hardlink.tar.gz"
            app = tarfile.TarInfo("Sigil.app")
            app.type = tarfile.DIRTYPE
            link = tarfile.TarInfo("Sigil.app/Contents/link")
            link.type = tarfile.LNKTYPE
            link.linkname = "../escape"
            write_archive(archive, [app, link])
            with self.assertRaisesRegex(ValueError, "link target escapes"):
                MODULE.extract_verified_archive(archive, root / "out")

    def test_existing_destination_is_not_deleted(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "safe.tar.gz"
            app = tarfile.TarInfo("Sigil.app")
            app.type = tarfile.DIRTYPE
            write_archive(archive, [app])
            destination = root / "out"
            destination.mkdir()
            sentinel = destination / "keep.txt"
            sentinel.write_text("keep", encoding="utf8")
            with self.assertRaisesRegex(ValueError, "must not already exist"):
                MODULE.extract_verified_archive(archive, destination)
            self.assertEqual(sentinel.read_text(encoding="utf8"), "keep")


if __name__ == "__main__":
    unittest.main()
