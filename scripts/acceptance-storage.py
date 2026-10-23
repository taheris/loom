#!/usr/bin/env python3
"""Read Nix database metadata, not every file in /nix/store. Never deletes."""
import argparse
import datetime
import json
import os
import pathlib
import shutil
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--dead', action='store_true', help='also ask native GC for dead paths and current roots (read-only)')
args = parser.parse_args()
repo = pathlib.Path.cwd()
parent = repo / '.loom/logs'
parent.mkdir(parents=True, exist_ok=True)
log = pathlib.Path(tempfile.mkdtemp(prefix='storage-' + datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ') + '-', dir=parent))
gib = 1024 ** 3
summary = {'logs': str(log), 'sizes': 'Nix NAR logical bytes from the store database, NOT physical du bytes or guaranteed reclaimed bytes', 'filesystems': {}}
summary['compiler_environment'] = {key: os.environ.get(key) for key in ['RUSTC', 'RUSTDOC', 'RUSTUP_TOOLCHAIN', 'RUSTC_WRAPPER', 'CARGO_BUILD_RUSTC', 'CARGO_TARGET_DIR', 'SCCACHE_DIR']}
for path in [repo, pathlib.Path('/nix/store')]:
    usage = shutil.disk_usage(path)
    summary['filesystems'][str(path)] = {'free_GiB': round(usage.free / gib, 2), 'total_GiB': round(usage.total / gib, 2)}

def capture(name, command):
    with (log / name).open('w') as out, (log / (name + '.stderr')).open('w') as err:
        result = subprocess.run(command, stdout=out, stderr=err, timeout=180)
    if result.returncode:
        raise SystemExit(f'{name} failed with exit {result.returncode}; inspect {log}')
    return log / name

config_file = capture('store-config.json', ['nix', '--extra-experimental-features', 'nix-command flakes', 'config', 'show', '--json'])
config = json.loads(config_file.read_text())
summary['keep_failed_builds'] = config.get('keep-failed', {}).get('value')
bases = {tempfile.gettempdir(), '/tmp', '/var/tmp', '/nix/var/nix/builds'}
configured = config.get('build-dir', {}).get('value')
if isinstance(configured, str):
    bases.add(configured)
summary['temporary_build_directories'] = []
summary['temporary_directory_inspection_errors'] = []
for base in sorted(bases):
    try:
        path = pathlib.Path(base)
        if not path.is_dir():
            continue
        for entry in sorted(path.iterdir()):
            if entry.name.startswith('nix-build-') and not entry.is_symlink() and entry.is_dir():
                summary['temporary_build_directories'].append(str(entry))
    except OSError as error:
        summary['temporary_directory_inspection_errors'].append({'path': base, 'error': str(error)})
summary['temporary_build_directory_warning'] = 'Names only, no recursive size scan. These paths may be active or contain diagnostics; they are NOT disposal authorization.'
info_file = capture('store-paths.json', ['nix', '--extra-experimental-features', 'nix-command flakes', 'path-info', '--all', '--json', '--json-format', '1'])
info = json.loads(info_file.read_text())
if not isinstance(info, dict) or any(not isinstance(v, dict) or (type(v.get('narSize')) is not int or v['narSize'] < 0) for v in info.values()):
    raise SystemExit(f'unexpected store metadata; inspect {info_file}; no zero-size fallback')
ranked = sorted(info, key=lambda p: info[p]['narSize'], reverse=True)
summary['store_paths'] = len(info)
summary['store_NAR_GiB'] = round(sum(v['narSize'] for v in info.values()) / gib, 2)
summary['largest_store_paths'] = [{'path': p, 'NAR_GiB': round(info[p]['narSize'] / gib, 3)} for p in ranked[:25]]
if args.dead:
    dead_file = capture('dead-paths', ['nix-store', '--gc', '--print-dead'])
    capture('gc-roots', ['nix-store', '--gc', '--print-roots'])
    dead = dead_file.read_text().splitlines()
    unknown = [p for p in dead if p not in info]
    summary['dead_paths'] = len(dead)
    summary['dead_paths_missing_from_metadata'] = unknown
    summary['dead_NAR_GiB'] = None if unknown else round(sum(info[p]['narSize'] for p in dead) / gib, 2)
    dead_set = set(dead)
    summary['largest_dead_paths'] = [{'path': p, 'NAR_GiB': round(info[p]['narSize'] / gib, 3)} for p in ranked if p in dead_set][:25]
(log / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
print(json.dumps(summary, indent=2))
