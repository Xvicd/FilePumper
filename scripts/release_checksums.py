"""Check the release assets and refresh the SHA-256 block in the release body.

Usage:
    python scripts/release_checksums.py              # download assets, print hashes
    python scripts/release_checksums.py --patch       # also rewrite the release body
    python scripts/release_checksums.py --tag v0.2.1 --notes docs/release-notes/v0.2.1.md --patch

ASCII-only source so it runs in any console code page; the notes themselves are
read and sent as UTF-8.
"""

from __future__ import annotations

import argparse
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
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


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
    parser = argparse.ArgumentParser(description="Check release assets and refresh their SHA-256 in the release body.")
    parser.add_argument("--tag", default="v0.3.0", help="release tag, e.g. v0.3.0")
    parser.add_argument("--notes", default=None, help="markdown template with {{WIN_SHA}}/{{LINUX_SHA}}")
    parser.add_argument("--patch", action="store_true", help="rewrite the release body from --notes")
    args = parser.parse_args()

    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")

    version = args.tag.lstrip("v")
    notes = args.notes or os.path.join(ROOT, "docs", "release-notes", f"{args.tag}.md")
    win_name = f"FilePumper-{version}-windows-x64.zip"
    linux_name = f"FilePumper-{version}-linux-x64.tar.gz"

    tok = token()
    release = api(f"https://api.github.com/repos/{REPO}/releases/tags/{args.tag}", tok)
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

    zip_path = os.path.join(workdir, win_name)
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

    if not args.patch:
        return 0

    with open(notes, encoding="utf-8") as handle:
        body = handle.read()
    body = body.replace("{{WIN_SHA}}", hashes[win_name])
    body = body.replace("{{LINUX_SHA}}", hashes[linux_name])
    payload = json.dumps({"body": body}, ensure_ascii=False).encode("utf-8")
    updated = api(f"https://api.github.com/repos/{REPO}/releases/{release['id']}", tok, "PATCH", payload)
    print("--- body ---")
    print(updated["body"])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
