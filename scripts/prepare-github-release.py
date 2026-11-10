#!/usr/bin/env python3
"""Prepare signed local release assets. Does not configure, upload or publish a release."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import tarfile


def run(args):
    return subprocess.run([str(a) for a in args], check=True, capture_output=True).stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repository', help='GitHub OWNER/REPO')
    parser.add_argument('--policy', type=Path, help='Release policy instead of explicit repository/compatibility arguments')
    parser.add_argument('--core', type=Path, required=True)
    parser.add_argument('--luci', type=Path, required=True)
    parser.add_argument('--sign-key', type=Path, required=True, help='Existing private release key; never copied into output')
    parser.add_argument('--upgrade-from', action='append', default=[], help='Explicitly tested compatible source version; repeat as needed')
    parser.add_argument('--openwrt-release', action='append', default=[], help='Explicitly tested compatible firmware release; repeat as needed')
    parser.add_argument('--output', type=Path, required=True, help='New directory')
    parser.add_argument('--apk', default='apk', help='APK v3 executable')
    parser.add_argument('--openssl', default='openssl')
    parser.add_argument('--linux-x86_64', type=Path)
    parser.add_argument('--linux-arm64', type=Path)
    args = parser.parse_args()
    if args.policy:
        if args.repository or args.upgrade_from or args.openwrt_release:
            parser.error('--policy cannot be combined with explicit policy values')
        policy = json.loads(args.policy.read_text())
        args.repository = policy['repository']
        args.upgrade_from = policy['upgrade_from']
        args.openwrt_release = policy['openwrt_releases']
    if not args.repository or not args.openwrt_release:
        parser.error('repository and compatible OpenWrt releases are required')
    if bool(args.linux_x86_64) != bool(args.linux_arm64):
        parser.error('both Linux architectures must be supplied together')
    if not re.fullmatch(r'[A-Za-z0-9_-][A-Za-z0-9_.-]*/[A-Za-z0-9_-][A-Za-z0-9_.-]*', args.repository):
        parser.error('repository must be OWNER/REPO')
    stable = r'(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})-r(0|[1-9][0-9]{0,8})'
    if any(not re.fullmatch(stable, v) for v in args.upgrade_from):
        parser.error('upgrade-from must use X.Y.Z-rN')
    if args.output.exists():
        parser.error('output directory already exists; choose a new directory')
    metadata = []
    for source, name, arch in [(args.core, 'cowplay', 'aarch64_generic'), (args.luci, 'luci-app-cowplay', 'noarch')]:
        info = json.loads(run([args.apk, '--allow-untrusted', 'adbdump', '--format', 'json', source.resolve()]))['info']
        if info['name'] != name or info['arch'] != arch or not re.fullmatch(stable, info['version']):
            parser.error(f'{name}: package name, architecture or version is invalid')
        metadata.append(info)
    if metadata[0]['version'] != metadata[1]['version']:
        parser.error('core and LuCI package versions must match')
    version = metadata[0]['version']
    linux_files = {}
    expected = {'cowplay', 'install.sh', 'cowplay.service', 'config.example.json', 'VERSION', 'ARCH', 'UPGRADE_FROM', 'LICENSE', 'NOTICE.md', 'TESLA-PROTOCOL-LICENSE', 'LIVI-NOTICE', 'README.md'}
    for arch, archive_path in [('x86_64', args.linux_x86_64), ('arm64', args.linux_arm64)]:
        if archive_path is None:
            continue
        root = f'cowplay-{version}-linux-{arch}'
        seen = set()
        with tarfile.open(archive_path, 'r:gz') as archive:
            for member in archive:
                name = member.name
                if name in seen:
                    parser.error('duplicate Linux archive member')
                seen.add(name)
                if name == root and member.isdir():
                    continue
                if name not in {root + '/' + n for n in expected} or not member.isfile() or member.size > 32 * 1024 * 1024:
                    parser.error('unexpected Linux archive member, type or size')
            if seen != {root} | {root + '/' + n for n in expected}:
                parser.error('Linux archive is incomplete')
            if archive.extractfile(root + '/VERSION').read().decode().strip() != version or archive.extractfile(root + '/ARCH').read().decode().strip() != arch:
                parser.error('Linux archive version or architecture mismatch')
            if archive.extractfile(root + '/UPGRADE_FROM').read().decode().split() != args.upgrade_from:
                parser.error('Linux archive upgrade compatibility differs from release policy')
            elf = archive.extractfile(root + '/cowplay').read(64)
            if elf[:7] != b'\x7fELF\x02\x01\x01' or int.from_bytes(elf[18:20], 'little') != (62 if arch == 'x86_64' else 183):
                parser.error('Linux archive executable architecture mismatch')
        linux_files[root + '.tar.gz'] = archive_path
    args.output.mkdir(mode=0o755, parents=True)
    with tempfile.TemporaryDirectory(prefix='cowplay-release-keys-') as raw:
        keys = Path(raw)
        public = run([args.openssl, 'pkey', '-in', args.sign_key.resolve(), '-pubout'])
        (keys / 'release.pem').write_bytes(public)
        (args.output / 'release-key.pem').write_bytes(public)
        packages = []
        for source, info in zip([args.core, args.luci], metadata):
            name = f"{info['name']}-{version}-{info['arch']}.apk"
            target = args.output / name
            shutil.copyfile(source, target)
            run([args.apk, '--allow-untrusted', 'adbsign', '--reset-signatures', '--sign-key', args.sign_key.resolve(), target.resolve()])
            run([args.apk, '--keys-dir', keys, '--no-network', 'verify', target.resolve()])
            packages.append(dict(name=info['name'], arch=info['arch'], file=name, size=target.stat().st_size, sha256=hashlib.file_digest(target.open('rb'), 'sha256').hexdigest()))
        manifest = dict(schema=1, repository=args.repository, tag='v' + version, version=version, platform='openwrt-apk', openwrt_releases=args.openwrt_release, arch='aarch64_generic', upgrade_from=args.upgrade_from, packages=packages)
        file = args.output / 'cowplay-release.json'
        file.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n')
        signature = args.output / 'cowplay-release.sig'
        run([args.openssl, 'dgst', '-sha256', '-sign', args.sign_key.resolve(), '-out', signature.resolve(), file.resolve()])
        run([args.openssl, 'dgst', '-sha256', '-verify', keys / 'release.pem', '-signature', signature.resolve(), file.resolve()])
        for name, source in linux_files.items():
            shutil.copyfile(source, args.output / name)
        if linux_files:
            shutil.copyfile(Path(__file__).resolve().parent / 'install-linux-from-github.sh', args.output / 'install-linux-from-github.sh')
        checksum_file = args.output / 'SHA256SUMS'
        checksums = []
        for file in sorted(args.output.iterdir()):
            with file.open('rb') as stream:
                checksums.append(hashlib.file_digest(stream, 'sha256').hexdigest() + '  ' + file.name)
        checksum_file.write_text('\n'.join(checksums) + '\n')
        checksum_sig = args.output / 'SHA256SUMS.sig'
        run([args.openssl, 'dgst', '-sha256', '-sign', args.sign_key.resolve(), '-out', checksum_sig.resolve(), checksum_file.resolve()])
        run([args.openssl, 'dgst', '-sha256', '-verify', keys / 'release.pem', '-signature', checksum_sig.resolve(), checksum_file.resolve()])
    print(json.dumps(dict(tag=manifest['tag'], output=str(args.output.resolve()), files=sorted(p.name for p in args.output.iterdir()), public_key_sha256=hashlib.sha256(public).hexdigest()), indent=2))


if __name__ == '__main__':
    try:
        main()
    except subprocess.CalledProcessError as error:
        raise SystemExit(f'Local {Path(error.cmd[0]).name} {error.cmd[1]} failed (exit {error.returncode}); incomplete output must not be published\n{error.stdout.decode(errors="replace")[:1000]}{error.stderr.decode(errors="replace")[:1000]}') from None
