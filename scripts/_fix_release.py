"""Download the release assets, check them and refresh the release body.

Usage: python scripts/_fix_release.py [--patch]
ASCII-only source: works in any console code page. Russian text comes from
release_notes.md, which is read and written as UTF-8.
"""

from __future__ import annotations

import hashlib
import json
import os
import struct
import subprocess
import sys
import tempfile
import urllib.request
import zipfile

REPO = "Xvicd/FilePumper"
TAG = "v0.2.0"
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
NOTES = os.path.join(ROOT, "release_notes.md")


def token() -> str:
    out = subprocess.run(
        ["git", "credential", "fill"],
        input="protocol=https\nhost=github.com\n\n",
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    for line in out.splitlines():
        if line.startswith("password="):
            return line.split("=", 1)[1].strip()
    raise SystemExit("no token from git credential fill")


def api(url: str, tok: str, method: str = "GET", data: bytes | None = None) -> dict:
    request = urllib.request.Request(url, method=method, data=data)
    request.add_header("Authorization", f"token {tok}")
    request.add_header("Accept", "application/vnd.github+json")
    request.add_header("User-Agent", "filepumper")
    if data is not None:
        request.add_header("Content-Type", "application/json; charset=utf-8")
    with urllib.request.urlopen(request) as response:
        return json.load(response)


def download(url: str, tok: str, path: str) -> None:
    request = urllib.request.Request(url)
    request.add_header("Authorization", f"token {tok}")
    request.add_header("Accept", "application/octet-stream")
    request.add_header("User-Agent", "filepumper")
    with urllib.request.urlopen(request) as response, open(path, "wb") as handle:
        handle.write(response.read())


def sha256(path: str) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def pe_subsystem(path: str) -> int:
    with open(path, "rb") as handle:
        data = handle.read(0x400)
    pe_offset = struct.unpack_from("<I", data, 0x3C)[0]
    optional = pe_offset + 24
    return struct.unpack_from("<H", data, optional + 68)[0]


def main() -> int:
    patch = "--patch" in sys.argv[1:]
    tok = token()
    release = api(f"https://api.github.com/repos/{REPO}/releases/tags/{TAG}", tok)
    workdir = os.path.join(tempfile.gettempdir(), "filepumper-release")
    os.makedirs(workdir, exist_ok=True)

    hashes: dict[str, str] = {}
    for asset in release["assets"]:
        name = asset["name"]
        path = os.path.join(workdir, name)
        download(asset["url"], tok, path)
        digest = sha256(path)
        hashes[name] = digest
        print(f"{name} | {os.path.getsize(path)} bytes | sha256 {digest}")

    zip_path = os.path.join(workdir, f"FilePumper-{TAG.lstrip('v')}-windows-x64.zip")
    if os.path.exists(zip_path):
        with zipfile.ZipFile(zip_path) as archive:
            for info in archive.infolist():
                print(f"  zip entry: {info.filename} ({info.file_size} bytes)")
            for member in ("FilePumper.exe", "FilePumper-cli.exe"):
                if member in archive.namelist():
                    extracted = os.path.join(workdir, member)
                    with archive.open(member) as source, open(extracted, "wb") as target:
                        target.write(source.read())
                    print(f"  {member}: PE subsystem {pe_subsystem(extracted)} (2=GUI, 3=console)")

    print(f"downloaded into {workdir}")

    if not patch:
        return 0

    with open(NOTES, encoding="utf-8") as handle:
        body = handle.read()
    body = body.replace("{{WIN_SHA}}", hashes[f"FilePumper-{TAG.lstrip('v')}-windows-x64.zip"])
    body = body.replace("{{LINUX_SHA}}", hashes[f"FilePumper-{TAG.lstrip('v')}-linux-x64.tar.gz"])
    payload = json.dumps({"body": body}, ensure_ascii=False).encode("utf-8")
    updated = api(f"https://api.github.com/repos/{REPO}/releases/{release['id']}", tok, "PATCH", payload)
    print("--- body ---")
    print(updated["body"])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
