#!/usr/bin/env python3
"""Install the locally built ccusage, preserving the previous executable for rollback."""
import argparse
import os
from pathlib import Path
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin-dir', type=Path, default=Path.home()/'.local/bin')
    parser.add_argument('--uninstall', action='store_true')
    args = parser.parse_args()
    directory = args.bin_dir.expanduser().resolve()
    target = directory/'ccusage'
    state = directory/'.ccusage-dsh-backup'
    if args.uninstall:
        if not target.is_symlink() or target.resolve() != (ROOT/'dist/ccusage').resolve():
            raise SystemExit('Refusing to replace ccusage: it is not this installation.')
        backup = Path(state.read_text().strip()) if state.exists() else None
        if backup is not None and not os.path.lexists(backup):
            raise SystemExit(f'Backup is missing: {backup}')
        target.unlink()
        if backup is not None:
            backup.rename(target)
            state.unlink()
        print(f'Restored previous ccusage at {target}')
        return
    binary = ROOT/'dist/ccusage'
    if not binary.is_file():
        raise SystemExit('Run python3 scripts/build.py first.')
    subprocess.run([str(binary),'--version'],check=True)
    directory.mkdir(parents=True,exist_ok=True)
    if target.is_symlink() and target.resolve() == binary.resolve():
        print(f'Already installed: {target}')
        return
    if os.path.lexists(target):
        if state.exists():
            raise SystemExit(f'An earlier backup exists: {state}; resolve it before installing.')
        backup = directory/f'ccusage.before-dsh.{time.time_ns()}'
        # Preserve symlinks as symlinks, including relative npm launcher targets.
        target.rename(backup)
        state.write_text(str(backup)+'\n')
        print(f'Previous ccusage preserved at {backup}')
    target.symlink_to(binary)
    print(f'Installed {target} -> {binary}')

if __name__ == '__main__':
    main()
