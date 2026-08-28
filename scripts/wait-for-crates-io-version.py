#!/usr/bin/env python3
"""Wait until one immutable crate version is visible through the crates.io API."""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


CRATE_NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$")
VERSION_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$")
API_ROOT = "https://crates.io/api/v1/crates"


def validate_inputs(name: str, version: str) -> list[str]:
    errors: list[str] = []
    if not CRATE_NAME_RE.fullmatch(name):
        errors.append("crate name is not a valid crates.io identifier")
    if not VERSION_RE.fullmatch(version):
        errors.append("version is not a valid bounded semver-like value")
    return errors


def version_is_visible(payload: object, version: str) -> bool:
    if not isinstance(payload, dict):
        return False
    versions = payload.get("versions")
    if not isinstance(versions, list):
        return False
    return any(
        isinstance(item, dict)
        and item.get("num") == version
        and item.get("yanked") is not True
        for item in versions
    )


def fetch_payload(name: str, timeout: float) -> object:
    request = Request(
        f"{API_ROOT}/{name}",
        headers={"Accept": "application/json", "User-Agent": "sigil-r70-release-train"},
    )
    with urlopen(request, timeout=timeout) as response:
        return json.load(response)


def wait_for_version(
    name: str,
    version: str,
    timeout_seconds: float,
    interval_seconds: float,
    request_timeout_seconds: float,
    fetch=fetch_payload,
    sleep=time.sleep,
    clock=time.monotonic,
) -> tuple[bool, str | None]:
    deadline = clock() + timeout_seconds
    last_error: str | None = None
    while True:
        try:
            if version_is_visible(fetch(name, request_timeout_seconds), version):
                return True, None
            last_error = "version is not visible or is yanked"
        except (HTTPError, URLError, TimeoutError, OSError, ValueError) as error:
            last_error = f"registry request failed: {error}"

        remaining = deadline - clock()
        if remaining <= 0:
            return False, last_error
        sleep(min(interval_seconds, remaining))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--name", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--timeout-seconds", type=float, default=300.0)
    parser.add_argument("--interval-seconds", type=float, default=5.0)
    parser.add_argument("--request-timeout-seconds", type=float, default=15.0)
    args = parser.parse_args(argv)

    errors = validate_inputs(args.name, args.version)
    if args.timeout_seconds <= 0 or args.timeout_seconds > 1800:
        errors.append("timeout-seconds must be in (0, 1800]")
    if args.interval_seconds <= 0 or args.interval_seconds > 60:
        errors.append("interval-seconds must be in (0, 60]")
    if args.request_timeout_seconds <= 0 or args.request_timeout_seconds > 60:
        errors.append("request-timeout-seconds must be in (0, 60]")
    if errors:
        for error in errors:
            print(f"r70 registry wait: {error}", file=sys.stderr)
        return 2

    visible, error = wait_for_version(
        args.name,
        args.version,
        args.timeout_seconds,
        args.interval_seconds,
        args.request_timeout_seconds,
    )
    if not visible:
        print(
            f"r70 registry wait: {args.name} {args.version} not visible before timeout"
            + (f" ({error})" if error else ""),
            file=sys.stderr,
        )
        return 2
    print(json.dumps({"name": args.name, "status": "visible", "version": args.version}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
