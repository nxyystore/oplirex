#!/usr/bin/env python3
"""Sync version across all packaging metadata from Cargo.toml truth."""

import re
import sys
import argparse
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CARGO = ROOT / "Cargo.toml"
AUR = ROOT / "platforms" / "arch" / "PKGBUILD"
AUR_BIN = ROOT / "platforms" / "arch" / "PKGBUILD-bin"
WINGET = ROOT / "platforms" / "windows" / "oplire.yaml"
WIX = ROOT / "platforms" / "windows" / "oplire.wxs"
NPM = ROOT / "platforms" / "npm" / "package.json"
SCOOP = ROOT / "platforms" / "scoop" / "oplirex.json"
CHOCO_NUSPEC = ROOT / "platforms" / "chocolatey" / "oplirex.nuspec"
CHOCO_PS1 = ROOT / "platforms" / "chocolatey" / "tools" / "chocolateyinstall.ps1"
HOMEBREW = ROOT / "platforms" / "homebrew" / "oplirex.rb"
README = ROOT / "README.md"

OWNER = "nxyystore"
REPO = "oplire"
PACKAGE_ID = "nxyy.oplirex"
PACKAGE_NAME = "oplirex"
PUBLISHER = "nxyy"


def read_version() -> str:
    text = CARGO.read_text(encoding="utf-8")
    match = re.search(r'^version\s*=\s*"([^"]+)"', text, re.MULTILINE)
    if not match:
        raise SystemExit("Could not find package version in Cargo.toml")
    return match.group(1).strip()


def set_cargo_version(version: str) -> bool:
    text = CARGO.read_text(encoding="utf-8")
    current = re.search(r'(?m)^version\s*=\s*"([^"]+)"', text)
    if not current:
        raise SystemExit("Failed to update Cargo.toml version")
    if current.group(1) == version:
        return False
    new = re.sub(r'(?m)^version\s*=\s*"[^"]+"', f'version = "{version}"', text, count=1)
    CARGO.write_text(new, encoding="utf-8")
    return True


def set_aur_version(version: str) -> None:
    for path in [AUR, AUR_BIN]:
        if not path.exists():
            continue
        text = path.read_text(encoding="utf-8")
        text = re.sub(r"(?m)^pkgver=.*", f"pkgver={version}", text, count=1)
        text = re.sub(
            r"(?m)^url=.*", f'url="https://github.com/{OWNER}/{REPO}"', text, count=1
        )
        path.write_text(text, encoding="utf-8")


def set_winget_version(version: str) -> None:
    if not WINGET.exists():
        return
    text = WINGET.read_text(encoding="utf-8")
    text = re.sub(
        r"(?m)^PackageVersion: .*$", f"PackageVersion: {version}", text, count=1
    )
    text = re.sub(
        r"(?m)^PackageIdentifier: .*$",
        f"PackageIdentifier: {PACKAGE_ID}",
        text,
        count=1,
    )
    text = re.sub(
        r"(?m)^PackageName: .*$", f"PackageName: {PACKAGE_NAME}", text, count=1
    )
    text = re.sub(r"(?m)^Publisher: .*$", f"Publisher: {PUBLISHER}", text, count=1)
    text = re.sub(
        r"(?m)^PublisherUrl: .*$",
        f"PublisherUrl: https://github.com/{OWNER}/{REPO}",
        text,
        count=1,
    )
    text = re.sub(r"(?m)^Author: .*$", f"Author: {PUBLISHER}", text, count=1)
    text = re.sub(
        r"(?m)^LicenseUrl: .*$",
        f"LicenseUrl: https://github.com/{OWNER}/{REPO}/blob/main/LICENSE",
        text,
        count=1,
    )
    text = re.sub(
        r"(?m)^PackageUrl: .*$",
        f"PackageUrl: https://github.com/{OWNER}/{REPO}",
        text,
        count=1,
    )
    text = re.sub(
        r"(?m)^\s*InstallerUrl: .*$",
        f"    InstallerUrl: https://github.com/{OWNER}/{REPO}/releases/download/v{version}/oplirex-windows.msi",
        text,
        count=1,
    )
    text = re.sub(
        r"(?m)^\s*ReleaseDate: .*$",
        f"    ReleaseDate: {date.today().isoformat()}",
        text,
        count=1,
    )
    WINGET.write_text(text, encoding="utf-8")


def set_wix_version(version: str) -> None:
    if not WIX.exists():
        return
    text = WIX.read_text(encoding="utf-8")
    # WiX Product Version="x.y.z"
    text = re.sub(r'(?m)Version="[^"]+"', f'Version="{version}"', text, count=1)
    WIX.write_text(text, encoding="utf-8")


def set_npm_version(version: str) -> None:
    if not NPM.exists():
        return
    import json

    data = json.loads(NPM.read_text(encoding="utf-8"))
    if data.get("version") != version:
        data["version"] = version
        NPM.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")


def set_scoop_version(version: str) -> None:
    if not SCOOP.exists():
        return
    import json

    data = json.loads(SCOOP.read_text(encoding="utf-8"))
    if data.get("version") != version:
        data["version"] = version
        # Update URLs to new version (hash left as placeholder, filled in CI post-build)
        for arch in data.get("architecture", {}).values():
            if isinstance(arch, dict) and "url" in arch:
                arch["url"] = re.sub(r"/v[0-9.]+/", f"/v{version}/", arch["url"])
        SCOOP.write_text(json.dumps(data, indent=4) + "\n", encoding="utf-8")


def set_choco_version(version: str) -> None:
    if CHOCO_NUSPEC.exists():
        text = CHOCO_NUSPEC.read_text(encoding="utf-8")
        new = re.sub(
            r"<version>[^<]+</version>", f"<version>{version}</version>", text, count=1
        )
        if new != text:
            CHOCO_NUSPEC.write_text(new, encoding="utf-8")
    if CHOCO_PS1.exists():
        text = CHOCO_PS1.read_text(encoding="utf-8")
        # Update version in URL if present
        new = re.sub(r"/v[0-9.]+/", f"/v{version}/", text)
        if new != text:
            CHOCO_PS1.write_text(new, encoding="utf-8")


def set_homebrew_version(version: str) -> None:
    if not HOMEBREW.exists():
        return
    text = HOMEBREW.read_text(encoding="utf-8")
    text = re.sub(r'version "[^"]+"', f'version "{version}"', text, count=1)
    text = re.sub(r"/v[0-9.]+/", f"/v{version}/", text)
    HOMEBREW.write_text(text, encoding="utf-8")


def set_readme_version(version: str) -> None:
    if not README.exists():
        return
    text = README.read_text(encoding="utf-8")
    # Update badge version if present
    new = re.sub(r"AUR-[0-9.]+-blue", f"AUR-{version}-blue", text)
    # Update Version: line in About section
    new = re.sub(r"(?m)^Version:.*", f"Version: {version}", new)
    if new != text:
        README.write_text(new, encoding="utf-8")


def check_versions(version: str) -> list[str]:
    """Return list of mismatches for --check mode."""
    mismatches = []
    current = read_version()
    if current != version:
        mismatches.append(f"Cargo.toml: {current} != {version}")
    for path, pattern in [
        (AUR, rf"pkgver={re.escape(version)}"),
        (WINGET, rf"PackageVersion: {re.escape(version)}"),
    ]:
        if path.exists() and not re.search(pattern, path.read_text(encoding="utf-8")):
            mismatches.append(f"{path.relative_to(ROOT)}: version mismatch")
    return mismatches


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Sync release version across packaging files"
    )
    parser.add_argument(
        "version",
        nargs="?",
        help="Version to set (e.g. 2.4.1 or v2.4.1). If omitted, uses Cargo.toml",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Check that all files match Cargo.toml version",
    )
    args = parser.parse_args()

    if args.check:
        version = read_version()
        mismatches = check_versions(version)
        if mismatches:
            for m in mismatches:
                print(f"MISMATCH: {m}", file=sys.stderr)
            return 1
        print(f"All files consistent at {version}")
        return 0

    version = args.version.strip().lstrip("v") if args.version else read_version()

    set_cargo_version(version)
    set_aur_version(version)
    set_winget_version(version)
    set_wix_version(version)
    set_npm_version(version)
    set_scoop_version(version)
    set_choco_version(version)
    set_homebrew_version(version)
    set_readme_version(version)

    print(f"Updated release metadata to version {version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
