"""Build the Windows x64 release and the NSIS installer.

    python scripts/package.py [--skip-build]

On Windows this uses the native MSVC target; elsewhere it cross-builds
x86_64-pc-windows-gnu (needs mingw-w64 and makensis on PATH). The installer
contents are the explicit list in STAGE below; nothing else is shipped.
"""
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WINDOWS = platform.system() == "Windows"
TRIPLE = None if WINDOWS else "x86_64-pc-windows-gnu"


def version():
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    return re.search(r'(?m)^version\s*=\s*"(\d+\.\d+\.\d+)"', text).group(1)


def target_dir():
    base = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    return base / TRIPLE / "release" if TRIPLE else base / "release"


def build():
    env = dict(os.environ)
    if TRIPLE:
        env.setdefault("WINDRES", "x86_64-w64-mingw32-windres")
    cmd = ["cargo", "build", "--release", "--locked", "-p", "shishi"]
    if TRIPLE:
        cmd += ["--target", TRIPLE]
    subprocess.run(cmd, cwd=ROOT, env=env, check=True)


def loader():
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--offline", "--locked", "--format-version", "1",
         "--filter-platform", TRIPLE or "x86_64-pc-windows-msvc"], cwd=ROOT,
    ))
    packages = [p for p in metadata["packages"] if p["name"] == "webview2-com-sys"]
    if len(packages) != 1:
        sys.exit("Expected one resolved webview2-com-sys package")
    dll = Path(packages[0]["manifest_path"]).parent / "x64/WebView2Loader.dll"
    if not dll.is_file():
        sys.exit("x64 WebView2Loader.dll missing from resolved package")
    return dll


def main():
    if "--skip-build" not in sys.argv:
        build()
    release = target_dir()
    out = ROOT / "build"
    stage = out / "stage"
    shutil.rmtree(stage, ignore_errors=True)
    stage.mkdir(parents=True)
    files = {
        "shishi.exe": release / "shishi.exe",
        "WebView2Loader.dll": loader(),
        "shishi.ico": ROOT / "assets/shishi.ico",
        "README.txt": ROOT / "README.txt",
        "CLI.md": ROOT / "docs/CLI.md",
        "SKILL.md": ROOT / "docs/skills/shishi-cli/SKILL.md",
        "LICENSE": ROOT / "LICENSE",
        "THIRD_PARTY_NOTICES.md": ROOT / "THIRD_PARTY_NOTICES.md",
        "LICENSE-WRY-MIT": ROOT / "vendor/wry/LICENSE-MIT",
        "LICENSE-WRY-APACHE": ROOT / "vendor/wry/LICENSE-APACHE",
    }
    for name, src in files.items():
        shutil.copyfile(src, stage / name)
    v = version()
    subprocess.run(
        ["makensis", "-V2", "-INPUTCHARSET", "UTF8", f"-DVERSION={v}", f"-DSTAGE={stage}",
         f"-DOUTPUT={out}", str(ROOT / "installer/setup.nsi")],
        check=True,
    )
    setup = out / f"Shishi-Setup-{v}.exe"
    digest = hashlib.sha256(setup.read_bytes()).hexdigest()
    print(f"{setup}\n{setup.stat().st_size} bytes\nsha256 {digest}")


if __name__ == "__main__":
    main()
