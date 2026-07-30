#!/usr/bin/env python3

from __future__ import annotations

import argparse
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import tarfile
import tempfile

MAX_MEMBERS = 20_000
MAX_REGULAR_BYTES = 2 * 1024 * 1024 * 1024
ARCHIVE_ROOT = PurePosixPath("Sigil.app")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Validate and extract one Sigil Desktop updater app archive"
    )
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--destination", required=True, type=Path)
    return parser.parse_args()


def normalized_member_path(name: str) -> PurePosixPath:
    path = PurePosixPath(name)
    if (
        not name
        or path.is_absolute()
        or any(part in {"", ".", ".."} for part in path.parts)
        or path.parts[0] != ARCHIVE_ROOT.name
    ):
        raise ValueError(f"unsafe updater archive member path: {name}")
    return path


def normalized_link_target(member: tarfile.TarInfo, path: PurePosixPath) -> PurePosixPath:
    target = PurePosixPath(member.linkname)
    if not member.linkname or target.is_absolute():
        raise ValueError(f"unsafe updater archive link target: {member.linkname}")
    if member.issym():
        combined = path.parent.joinpath(target)
    else:
        combined = target
    normalized_parts: list[str] = []
    for part in combined.parts:
        if part in {"", "."}:
            continue
        if part == "..":
            if not normalized_parts:
                raise ValueError(
                    f"updater archive link target escapes Sigil.app: {member.linkname}"
                )
            normalized_parts.pop()
        else:
            normalized_parts.append(part)
    normalized = PurePosixPath(*normalized_parts)
    if not normalized.parts or normalized.parts[0] != ARCHIVE_ROOT.name:
        raise ValueError(
            f"updater archive link target escapes Sigil.app: {member.linkname}"
        )
    return normalized


def validate_members(members: list[tarfile.TarInfo]) -> None:
    if not members or len(members) > MAX_MEMBERS:
        raise ValueError("updater archive has an unsafe member count")
    seen: set[PurePosixPath] = set()
    regular_bytes = 0
    for member in members:
        path = normalized_member_path(member.name)
        if path in seen:
            raise ValueError(f"duplicate updater archive member path: {member.name}")
        seen.add(path)
        if member.isdev() or member.isfifo():
            raise ValueError(f"unsupported updater archive member type: {member.name}")
        if member.isfile():
            regular_bytes += member.size
            if regular_bytes > MAX_REGULAR_BYTES:
                raise ValueError("updater archive expands beyond the safety limit")
        if member.issym() or member.islnk():
            normalized_link_target(member, path)


def validate_extracted_tree(destination: Path) -> Path:
    app_path = destination / ARCHIVE_ROOT.name
    app_stat = app_path.lstat()
    if not stat.S_ISDIR(app_stat.st_mode) or stat.S_ISLNK(app_stat.st_mode):
        raise ValueError("updater archive did not produce a regular Sigil.app directory")
    resolved_root = app_path.resolve(strict=True)
    for directory, directory_names, file_names in os.walk(app_path, followlinks=False):
        for name in [*directory_names, *file_names]:
            candidate = Path(directory, name)
            if candidate.is_symlink():
                resolved = candidate.resolve(strict=True)
                if os.path.commonpath([resolved_root, resolved]) != str(resolved_root):
                    raise ValueError(
                        f"extracted updater archive symlink escapes Sigil.app: {candidate}"
                    )
    return app_path


def extract_verified_archive(archive_path: Path, destination: Path) -> Path:
    archive_stat = archive_path.lstat()
    if not stat.S_ISREG(archive_stat.st_mode) or stat.S_ISLNK(archive_stat.st_mode):
        raise ValueError(f"updater archive must be a regular file: {archive_path}")
    if destination.exists():
        raise ValueError(f"extraction destination must not already exist: {destination}")
    parent_stat = destination.parent.lstat()
    if not stat.S_ISDIR(parent_stat.st_mode) or stat.S_ISLNK(parent_stat.st_mode):
        raise ValueError(
            f"extraction destination parent must be a regular directory: {destination.parent}"
        )

    staging = Path(
        tempfile.mkdtemp(
            prefix=f".{destination.name}.extract-",
            dir=destination.parent,
        )
    )
    try:
        with tarfile.open(archive_path, mode="r:gz") as archive:
            members = archive.getmembers()
            validate_members(members)
            if not hasattr(tarfile, "data_filter"):
                raise RuntimeError("Python tarfile data extraction filter is required")
            archive.extractall(staging, members=members, filter="data")
        validate_extracted_tree(staging)
        staging.rename(destination)
    except BaseException:
        shutil.rmtree(staging, ignore_errors=True)
        raise
    return validate_extracted_tree(destination)


def main() -> int:
    args = parse_args()
    try:
        app_path = extract_verified_archive(
            args.archive.absolute(),
            args.destination.absolute(),
        )
    except (OSError, tarfile.TarError, ValueError, RuntimeError) as error:
        print(error, file=os.sys.stderr)
        return 1
    print(app_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
