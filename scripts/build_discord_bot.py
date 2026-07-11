"""Build the bundled Discord bot runtime on Windows or macOS."""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path


def run(*args: str) -> None:
    subprocess.run(args, check=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--clean", action="store_true")
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parent.parent
    source_dir = repo_root / "runtime" / "discord-bot-src"
    build_root = repo_root / "runtime" / ".build" / "discord-bot"
    venv_dir = build_root / ".venv"
    work_dir = build_root / "work"
    spec_dir = build_root / "spec"
    dist_root = build_root / "dist"
    app_dist_dir = dist_root / "xiaokui_bot"
    bundle_dir = repo_root / "src-tauri" / "resources" / "discord-bot"
    entry_script = source_dir / "bot_xiaokui.py"
    requirements = source_dir / "requirements.txt"
    config_example = source_dir / "config_xiaokui.example.json"
    env_example = source_dir / ".env.xiaokui.example"
    venv_python = venv_dir / ("Scripts/python.exe" if os.name == "nt" else "bin/python")

    if not entry_script.is_file():
        raise FileNotFoundError(f"Discord bot entry point not found: {entry_script}")

    if args.clean:
        shutil.rmtree(build_root, ignore_errors=True)
        shutil.rmtree(bundle_dir, ignore_errors=True)

    if venv_dir.exists() and not venv_python.is_file():
        shutil.rmtree(venv_dir)
    if not venv_python.is_file():
        run(sys.executable, "-m", "venv", str(venv_dir))

    work_dir.mkdir(parents=True, exist_ok=True)
    spec_dir.mkdir(parents=True, exist_ok=True)
    dist_root.mkdir(parents=True, exist_ok=True)

    run(str(venv_python), "-m", "pip", "install", "--upgrade", "pip")
    run(
        str(venv_python),
        "-m",
        "pip",
        "install",
        "-r",
        str(requirements),
        "pyinstaller",
    )
    run(
        str(venv_python),
        "-m",
        "PyInstaller",
        "--noconfirm",
        "--clean",
        "--onedir",
        "--name",
        "xiaokui_bot",
        "--distpath",
        str(dist_root),
        "--workpath",
        str(work_dir),
        "--specpath",
        str(spec_dir),
        str(entry_script),
    )

    if not app_dist_dir.is_dir():
        raise FileNotFoundError(f"PyInstaller output not found: {app_dist_dir}")

    shutil.rmtree(bundle_dir, ignore_errors=True)
    shutil.copytree(app_dist_dir, bundle_dir)
    shutil.copy2(config_example, bundle_dir / "config_xiaokui.json")
    shutil.copy2(env_example, bundle_dir / ".env.xiaokui.example")
    print(f"Discord Bot runtime written to: {bundle_dir}")


if __name__ == "__main__":
    main()
