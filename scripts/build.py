#!/usr/bin/env python3
"""Materialize a clean pinned upstream, apply the DSH overlay, and build locally."""
import argparse
import os
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
BUILD = ROOT / '.build'
SOURCE = BUILD / 'source'

def run(*args, **kwargs):
    subprocess.run(args, check=True, **kwargs)

def prepare():
    upstream = ROOT / 'upstream'
    run('git', '-C', str(ROOT), 'submodule', 'update', '--init')
    if SOURCE.exists():
        shutil.rmtree(SOURCE)
    SOURCE.mkdir(parents=True)
    with tempfile.TemporaryFile() as archive:
        run('git', '-C', str(upstream), 'archive', 'HEAD', stdout=archive)
        archive.seek(0)
        run('tar', '-xf', '-', '-C', str(SOURCE), stdin=archive)
    run('git', 'init', '--quiet', str(SOURCE))
    run('git', 'apply', '--check', str(ROOT / 'patches/dsh.patch'), cwd=SOURCE)
    run('git', 'apply', str(ROOT / 'patches/dsh.patch'), cwd=SOURCE)
    shutil.copytree(ROOT / 'adapter', SOURCE / 'rust/adapters/dsh')
    package_path = SOURCE / 'package.json'
    package = json.loads(package_path.read_text())
    package['version'] += '+dsh.1'
    package_path.write_text(json.dumps(package, indent='\t')+'\n')
    # Keep dependency resolution committed alongside the overlay, not in the submodule.
    lock = ROOT / 'Cargo.lock'
    if lock.exists():
        shutil.copy2(lock, SOURCE / 'rust/Cargo.lock')

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--test', action='store_true')
    parser.add_argument('--prepare-only', action='store_true')
    args = parser.parse_args()
    prepare()
    if args.prepare_only:
        return
    env = dict(os.environ, CARGO_TARGET_DIR=str(BUILD / 'target'), CCUSAGE_VERSION=json.loads((SOURCE/'package.json').read_text())['version'])
    if args.test:
        env['DSH_HOME'] = str(BUILD/'empty-dsh-home')
    command = ['cargo', 'test' if args.test else 'build', '--manifest-path', 'rust/Cargo.toml']
    if args.test:
        command += ['-p', 'ccusage-adapter-dsh', '-p', 'ccusage-cli-parser', '-p', 'ccusage-adapter-all', '-p', 'ccusage']
    else:
        command += ['--release', '-p', 'ccusage']
    command += ['--features', 'ccusage/fetch-litellm-pricing']
    if (ROOT / 'Cargo.lock').exists():
        command += ['--locked']
    run(*command, cwd=SOURCE, env=env)
    if not args.test:
        dist = ROOT / 'dist'
        dist.mkdir(exist_ok=True)
        shutil.copy2(BUILD / 'target/release/ccusage', dist / 'ccusage')
        print(f'Built {dist / "ccusage"}')

if __name__ == '__main__':
    main()
