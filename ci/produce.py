#!/usr/bin/env python3
"""Produce cross-host test archives with the *host* tar.

Builds a source tree with the shapes that differ between tar implementations
and platforms, records ground truth about it in manifest.json, archives it
with the host `tar` (default flags, plus a PAX and, on Linux, an xattrs/ACL
variant where supported), and wraps each tar with the given tarzan binary.

    python3 ci/produce.py OUTDIR path/to/tarzan

The consumer side is tests/cross_host.rs, run with CROSS_HOST_DIR pointing at
a directory containing one or more OUTDIRs (CI downloads each producer's
artifact into its own subdirectory).
"""

import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path

LONG = "a" * 60


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def try_run(cmd, **kw) -> bool:
    try:
        subprocess.run(cmd, check=True, capture_output=True, **kw)
        return True
    except (OSError, subprocess.CalledProcessError):
        return False


def build_tree(src: Path, skipped: list) -> dict:
    """Create the source tree; return manifest fragments."""
    src.mkdir(parents=True)
    (src / "plain.txt").write_bytes(b"hello\n")
    (src / "empty.txt").write_bytes(b"")
    (src / "run.sh").write_bytes(b"#!/bin/sh\necho hi\n")
    os.chmod(src / "run.sh", 0o755)
    (src / "dir with spaces" / "nested").mkdir(parents=True)
    (src / "dir with spaces" / "nested" / "file.txt").write_bytes(b"nested\n")
    (src / "emptydir").mkdir()
    (src / f"{LONG}" / f"{LONG}").mkdir(parents=True)
    long_file = src / LONG / LONG / f"deep_{LONG}.txt"
    long_file.write_bytes(b"deep\n")
    (src / "Ärger_naïve_日本語.txt").write_bytes(b"umlaut\n")
    # Deterministic multi-chunk payload (6 MiB) so members span frames.
    (src / "big.bin").write_bytes(bytes((i * 7919) % 251 for i in range(6 * 1024 * 1024)))

    # Sub-second mtime on one file.
    os.utime(src / "plain.txt", ns=(1_700_000_000_123_456_789, 1_700_000_000_123_456_789))

    symlinks = []
    try:
        os.symlink("plain.txt", src / "symlink.txt")
        symlinks.append({"path": "./symlink.txt", "target": "plain.txt"})
        rel_long = str(long_file.relative_to(src)).replace(os.sep, "/")
        os.symlink(rel_long, src / "longlink")
        symlinks.append({"path": "./longlink", "target": rel_long})
    except OSError as e:
        skipped.append(f"symlink: {e}")

    hardlinks = []
    try:
        os.link(src / "plain.txt", src / "hardlink.txt")
        hardlinks.append(["./plain.txt", "./hardlink.txt"])
    except OSError as e:
        skipped.append(f"hardlink: {e}")

    xattrs = []
    system = platform.system()
    if system == "Linux":
        if try_run(["setfattr", "-n", "user.note", "-v", "plain xattr", str(src / "plain.txt")]):
            xattrs.append({"path": "./plain.txt", "name": "user.note", "value": "plain xattr"})
        else:
            skipped.append("xattr: setfattr unavailable or unsupported")
    elif system == "Darwin":
        if try_run(["xattr", "-w", "user.note", "plain xattr", str(src / "plain.txt")]):
            xattrs.append({"path": "./plain.txt", "name": "user.note", "value": "plain xattr"})
        fork = b"resource fork bytes\n\x00\x01\xff"
        if try_run(["xattr", "-wx", "com.apple.ResourceFork", fork.hex(), str(src / "run.sh")]):
            xattrs.append({"path": "./run.sh", "name": "com.apple.ResourceFork", "value_hex": fork.hex()})
    else:
        skipped.append("xattr: no xattrs on this platform")

    files = []
    for p in sorted(src.rglob("*")):
        if p.is_symlink() or not p.is_file():
            continue
        rel = "./" + str(p.relative_to(src)).replace(os.sep, "/")
        if any(rel == pair[1] for pair in hardlinks):
            continue  # the link side is recorded under hardlinks
        files.append({"path": rel, "size": p.stat().st_size, "sha256": sha256(p)})
    return {"files": files, "symlinks": symlinks, "hardlinks": hardlinks, "xattrs": xattrs}


def tar_variants(system: str) -> list:
    """(name, extra flags) per variant the host tar might support."""
    variants = [("default", [])]
    variants.append(("pax", ["--format=pax"]))
    if system == "Linux":
        variants.append(("aggressive", ["--xattrs", "--acls", "--format=pax"]))
    return variants


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    out = Path(sys.argv[1]).resolve()
    tarzan = Path(sys.argv[2]).resolve()
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    src = out / "src"
    skipped: list = []
    manifest = build_tree(src, skipped)

    system = platform.system()
    env = dict(os.environ)
    env.pop("COPYFILE_DISABLE", None)  # we want macOS's real default output

    tar_version = subprocess.run(["tar", "--version"], capture_output=True, text=True).stdout.splitlines()[0]
    archives = []
    for name, flags in tar_variants(system):
        tar_path = out / f"{name}.tar"
        cmd = ["tar", "-cf", str(tar_path), *flags, "-C", str(src), "."]
        r = subprocess.run(cmd, capture_output=True, text=True, env=env)
        if r.returncode != 0:
            skipped.append(f"tar variant {name}: {r.stderr.strip()}")
            tar_path.unlink(missing_ok=True)
            continue
        zst = out / f"{name}.tar.zst"
        subprocess.run([str(tarzan), "wrap", str(tar_path), "--chunk-size", "1M", "-f", str(zst)], check=True)
        archives.append({"name": name, "tar": tar_path.name, "tarzan": zst.name, "flags": flags,
                         "tar_stderr": r.stderr.strip()})

    manifest.update({
        "producer": {"system": system, "release": platform.release(), "machine": platform.machine(),
                     "tar": tar_version, "python": platform.python_version()},
        "archives": archives,
        "skipped": skipped,
    })
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    # The source tree is not needed downstream and would carry host xattrs.
    shutil.rmtree(src)
    print(f"produced {len(archives)} archive(s) in {out}; skipped: {skipped or 'nothing'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
