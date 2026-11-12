#!/usr/bin/env python3
"""Actual APK v3 signatures/metadata/install in a disposable root; never operates a device."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import io
import tarfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--apk', default='apk')
parser.add_argument('--openssl', default='openssl')
args = parser.parse_args()
project = Path(__file__).resolve().parent.parent


def run(argv, success=True):
    result = subprocess.run([str(x) for x in argv], capture_output=True)
    assert (result.returncode == 0) == success, (argv[0:3], result.stderr.decode())
    return result.stdout


with tempfile.TemporaryDirectory(prefix='release-artifacts-', dir=project / 'target') as raw:
    tmp = Path(raw)
    key = tmp / 'test-private.key'
    run([args.openssl, 'genpkey', '-algorithm', 'RSA', '-pkeyopt', 'rsa_keygen_bits:2048', '-out', key])
    inputs = []
    for name, arch in [('cowplay', 'aarch64_generic'), ('luci-app-cowplay', 'noarch')]:
        files = tmp / name
        (files / 'usr/share' / name).mkdir(parents=True)
        (files / 'usr/share' / name / 'synthetic.txt').write_text('Synthetic release test only')
        apk = tmp / (name + '.apk')
        run([args.apk, 'mkpkg', '--info', 'name:' + name, '--info', 'version:0.3.15-r0', '--info', 'arch:' + arch, '--files', files, '--output', apk])
        inputs.append(apk)
    linux = []
    linux_names = 'cowplay install.sh cowplay.service config.example.json VERSION ARCH UPGRADE_FROM LICENSE NOTICE.md TESLA-PROTOCOL-LICENSE LIVI-NOTICE README.md'.split()
    for arch in ['x86_64', 'arm64']:
        archive_path = tmp / f'cowplay-0.3.15-r0-linux-{arch}.tar.gz'
        root_name = f'cowplay-0.3.15-r0-linux-{arch}'
        with tarfile.open(archive_path, 'w:gz', format=tarfile.USTAR_FORMAT) as archive:
            directory = tarfile.TarInfo(root_name + '/'); directory.type = tarfile.DIRTYPE; archive.addfile(directory)
            for name in linux_names:
                if name == 'cowplay':
                    data = bytearray(120); data[:7] = b'\x7fELF\x02\x01\x01'; data[18:20] = (62 if arch == 'x86_64' else 183).to_bytes(2, 'little'); data = bytes(data)
                else:
                    data = {'VERSION': b'0.3.15-r0\n', 'ARCH': (arch + '\n').encode(), 'UPGRADE_FROM': b'0.3.14-r16\n'}.get(name, b'synthetic archive fixture')
                member = tarfile.TarInfo(root_name + '/' + name); member.size = len(data); archive.addfile(member, io.BytesIO(data))
        linux.append(archive_path)
    output = tmp / 'assets'
    notes = tmp / 'release-notes.md'
    run(['python3', project / 'scripts/prepare-github-release.py', '--repository', 'example/cowplay', '--channel', 'dev', '--core', inputs[0], '--luci', inputs[1], '--sign-key', key, '--upgrade-from', '0.3.14-r16', '--openwrt-release', '25.12.6', '--output', output, '--notes', notes, '--apk', args.apk, '--openssl', args.openssl, '--linux-x86_64', linux[0], '--linux-arm64', linux[1]])
    expected_assets = {
        'cowplay-0.3.15-r0-aarch64_generic.apk', 'luci-app-cowplay-0.3.15-r0-noarch.apk',
        *(p.name for p in linux), 'release-key.pem', 'cowplay-release.json', 'cowplay-release.sig',
        'SHA256SUMS', 'SHA256SUMS.sig', 'install-linux-from-github.sh',
        'install-openwrt-from-github.sh', 'cowplay-updater-aarch64',
    }
    assert {p.name for p in output.iterdir()} == expected_assets
    body = notes.read_text()
    assert '# CowPlay v0.3.15' in body and '仍需实机验收' in body and '开发预发布' in body
    assert str(tmp) not in body and key.read_text() not in body
    script_hash = hashlib.sha256((output / 'install-openwrt-from-github.sh').read_bytes()).hexdigest()
    fingerprint = hashlib.sha256((output / 'release-key.pem').read_bytes()).hexdigest()
    assert script_hash in body and fingerprint in body
    core_only = tmp / 'core-assets'
    core_notes = tmp / 'core-notes.md'
    run(['python3', project / 'scripts/prepare-github-release.py', '--repository', 'example/cowplay', '--channel', 'stable', '--core', inputs[0], '--luci', inputs[1], '--sign-key', key, '--openwrt-release', '25.12.6', '--output', core_only, '--notes', core_notes, '--apk', args.apk, '--openssl', args.openssl])
    assert 'tar.gz' not in core_notes.read_text() and 'LuCI 首次安装命令' not in core_notes.read_text()
    assert '**正式版**' in core_notes.read_text() and '开发预发布' not in core_notes.read_text()
    rejected_output = tmp / 'rejected-assets'
    run(['python3', project / 'scripts/prepare-github-release.py', '--repository', 'example/cowplay', '--channel', 'dev', '--core', inputs[0], '--luci', inputs[1], '--sign-key', key, '--openwrt-release', '25.12.6', '--output', rejected_output, '--notes', rejected_output / 'notes.md', '--apk', args.apk, '--openssl', args.openssl], success=False)
    assert not rejected_output.exists()
    assert key.read_bytes() not in b''.join(p.read_bytes() for p in output.iterdir())
    manifest = json.loads((output / 'cowplay-release.json').read_bytes())
    keys = tmp / 'keys'
    keys.mkdir()
    (keys / 'release.pem').write_bytes((output / 'release-key.pem').read_bytes())
    run([args.openssl, 'dgst', '-sha256', '-verify', keys / 'release.pem', '-signature', output / 'cowplay-release.sig', output / 'cowplay-release.json'])
    run([args.openssl, 'dgst', '-sha256', '-verify', keys / 'release.pem', '-signature', output / 'SHA256SUMS.sig', output / 'SHA256SUMS'])
    covered = set()
    for line in (output / 'SHA256SUMS').read_text().splitlines():
        digest, name = line.split('  ', 1)
        assert name not in covered and '/' not in name
        covered.add(name)
        assert hashlib.sha256((output / name).read_bytes()).hexdigest() == digest
    assert all(path.name in covered for path in linux)
    assert 'install-linux-from-github.sh' in covered
    assert covered == expected_assets - {'SHA256SUMS', 'SHA256SUMS.sig'}
    packages = []
    for pkg in manifest['packages']:
        file = output / pkg['file']
        assert file.stat().st_size == pkg['size']
        assert hashlib.sha256(file.read_bytes()).hexdigest() == pkg['sha256']
        run([args.apk, '--keys-dir', keys, '--no-network', 'verify', file])
        info = json.loads(run([args.apk, '--keys-dir', keys, '--no-network', 'adbdump', '--format', 'json', file]))['info']
        assert (info['name'], info['version'], info['arch']) == (pkg['name'], manifest['version'], pkg['arch'])
        packages.append(file)
    root = tmp / 'install-root'
    root.mkdir()
    run([args.apk, '--root', root, '--arch', 'aarch64_generic', '--no-network', '--no-scripts', 'add', '--initdb'])
    run([args.apk, '--root', root, '--no-network', 'verify', *packages], success=False)
    system_keys = root / 'etc/apk/keys'
    system_keys.mkdir(parents=True, exist_ok=True)
    (system_keys / 'cowplay-release.pem').write_bytes((output / 'release-key.pem').read_bytes())
    run([args.apk, '--root', root, '--no-network', 'verify', *packages])
    run([args.apk, '--root', root, '--arch', 'aarch64_generic', '--no-network', '--no-cache', '--repositories-file', '/dev/null', '--no-scripts', 'add', '--initdb', '--simulate', *packages, 'cowplay=0.3.15-r0', 'luci-app-cowplay=0.3.15-r0'])
    run([args.apk, '--root', root, '--arch', 'aarch64_generic', '--no-network', '--no-cache', '--repositories-file', '/dev/null', '--no-scripts', 'add', '--initdb', *packages, 'cowplay=0.3.15-r0', 'luci-app-cowplay=0.3.15-r0'])
    rows = json.loads(run([args.apk, '--root', root, '--no-network', 'query', '--installed', '--format', 'json', '--fields', 'name,version', 'cowplay', 'luci-app-cowplay']))
    assert sorted((p['name'], p['version']) for p in rows) == [('cowplay', '0.3.15-r0'), ('luci-app-cowplay', '0.3.15-r0')]
    other = tmp / 'other.key'
    run([args.openssl, 'genpkey', '-algorithm', 'RSA', '-pkeyopt', 'rsa_keygen_bits:2048', '-out', other])
    (keys / 'release.pem').write_bytes(run([args.openssl, 'pkey', '-in', other, '-pubout']))
    run([args.apk, '--keys-dir', keys, '--no-network', 'verify', packages[0]], success=False)
    (keys / 'release.pem').write_bytes((output / 'release-key.pem').read_bytes())
    run([args.apk, '--keys-dir', keys, '--no-network', 'verify', inputs[0]], success=False)
    (output / 'cowplay-release.json').write_text('{}')
    run([args.openssl, 'dgst', '-sha256', '-verify', keys / 'release.pem', '-signature', output / 'cowplay-release.sig', output / 'cowplay-release.json'], success=False)
print('SIGNED_APK_LINUX_RELEASE_CHECKSUMS_FIRST_INSTALL_AND_TAMPER_REJECTION_PASSED')
