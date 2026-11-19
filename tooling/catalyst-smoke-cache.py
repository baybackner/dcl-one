#!/usr/bin/env python3
"""Drop superseded Rust build artifacts before saving the CI smoke build snapshot."""
import json
from pathlib import Path
import re
import subprocess


def prune(deps, messages):
    # Rust library metadata and its rlib share a stem even when Cargo lists only one.
    library = re.compile(r"(lib.+)-[0-9a-f]{16}\.(rlib|rmeta|so|dylib)")
    executable = re.compile(r"catalyst-[0-9a-f]{16}")
    app = deps.parent / "catalyst"
    reported = set()
    for line in messages.splitlines():
        artifact = json.loads(line)
        if artifact["reason"] == "compiler-artifact":
            reported.update(Path(name) for name in artifact["filenames"])

    def family(match):
        suffix = match[2]
        return match[1], "rlib" if suffix == "rmeta" else suffix

    active = {}
    for path in reported:
        match = library.fullmatch(path.name)
        if path.parent == deps and match:
            active.setdefault(family(match), set()).add(path.stem)
    if not active:
        raise ValueError("Cargo reported no Rust libraries in the smoke target")

    # Cargo reports debug/catalyst. Only its hard link identifies the active hashed binary.
    binaries = [p for p in deps.glob("catalyst-*") if executable.fullmatch(p.name)
                and not p.is_symlink() and p.is_file()]
    retained = {p for p in binaries if app in reported and app.is_file() and p.samefile(app)}
    obsolete = set(binaries) - retained - reported if retained else set()
    for path in deps.iterdir():
        match = library.fullmatch(path.name)
        if match and (stems := active.get(family(match))) and path.stem not in stems:
            obsolete.add(path)
    removed = 0
    for path in obsolete:
        removed += path.stat().st_size
        path.unlink()
    return removed


if __name__ == "__main__":
    workspace = Path(__file__).resolve().parents[1] / "services/catalyst"
    target = workspace / "target"
    result = subprocess.check_output(
        ["cargo", "build", "--locked", "--bin", "catalyst", "--target-dir", str(target),
         "--message-format=json-render-diagnostics"], cwd=workspace, text=True)
    removed = prune(target / "debug/deps", result)
    print(f"Removed {removed:,} bytes of superseded Rust build artifacts from the smoke snapshot.")
