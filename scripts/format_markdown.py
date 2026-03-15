#!/usr/bin/env python3
"""Format all .md and .mdx files in the repository using Prettier."""

import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
EXTENSIONS = ("*.md", "*.mdx")


def main() -> int:
    files = []
    for ext in EXTENSIONS:
        files.extend(REPO_ROOT.rglob(ext))

    if not files:
        print("No .md/.mdx files found.")
        return 0

    print(f"Formatting {len(files)} file(s)...")
    result = subprocess.run(
        ["npx", "--yes", "prettier", "--write", "--prose-wrap", "preserve", *files],
        cwd=REPO_ROOT,
        shell=True,
    )
    return result.returncode


if __name__ == "__main__":
    sys.exit(main())
