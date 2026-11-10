#!/usr/bin/env python3
"""Build a deterministic Linux archive from a validated static executable."""
import argparse
import gzip
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile

project = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('binary', type=Path)
parser.add_argument('arch', choices=['x86_64', 'arm64'])
parser.add_argument('output', type=Path)
parser.add_argument('--checker', type=Path, help='Native CowPlay ELF checker, for cross packaging')
args = parser.parse_args()
version = re.search(r'--info version:([^\s]+)', (project / 'scripts/build-apk.sh').read_text()).group(1)
assert re.fullmatch(r'(0|[1-9][0-9]{0,8})(\.(0|[1-9][0-9]{0,8})){2}-r(0|[1-9][0-9]{0,8})', version)
policy = json.loads((project / 'packaging/release-policy.json').read_text())
subprocess.run([str((args.checker or args.binary).resolve()), 'verify-image', str(args.binary.resolve()), args.arch], check=True)
root = f'cowplay-{version}-linux-{args.arch}'
files = {
    'cowplay': (args.binary, 0o755),
    'install.sh': (project / 'packaging/linux/install.sh', 0o755),
    'cowplay.service': (project / 'packaging/cowplay.service', 0o644),
    'config.example.json': (project / 'packaging/config.linux.example.json', 0o644),
    'LICENSE': (project / 'LICENSE', 0o644),
    'NOTICE.md': (project / 'NOTICE.md', 0o644),
    'TESLA-PROTOCOL-LICENSE': (project / 'packaging/TESLA-PROTOCOL-LICENSE', 0o644),
    'LIVI-NOTICE': (project / 'crates/server/src/ultra/livi/NOTICE', 0o644),
    'README.md': (project / 'packaging/linux/README.md', 0o644),
}
contents = {name: (path.read_bytes(), mode) for name, (path, mode) in files.items() if not path.is_symlink()}
assert len(contents) == len(files), 'archive inputs must be regular files, not symlinks'
contents.update(VERSION=((version + '\n').encode(), 0o644), ARCH=((args.arch + '\n').encode(), 0o644), UPGRADE_FROM=(('\n'.join(policy['upgrade_from']) + '\n').encode(), 0o644))
if args.output.exists():
    parser.error('output already exists; choose a new path')
args.output.parent.mkdir(parents=True, exist_ok=True)
with args.output.open('xb') as raw, gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0) as compressed, tarfile.open(fileobj=compressed, mode='w', format=tarfile.USTAR_FORMAT) as archive:
    directory = tarfile.TarInfo(root + '/')
    directory.type = tarfile.DIRTYPE
    directory.mode = 0o755
    archive.addfile(directory)
    for name, (data, mode) in sorted(contents.items()):
        entry = tarfile.TarInfo(root + '/' + name)
        entry.size, entry.mode = len(data), mode
        archive.addfile(entry, io.BytesIO(data))
print(json.dumps(dict(version=version, arch=args.arch, archive=str(args.output), files=sorted(contents)), indent=2))
