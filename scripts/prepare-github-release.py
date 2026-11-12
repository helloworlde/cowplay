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


def bootstrap_command(repository, tag, version, script_hash, fingerprint, channel='stable'):
    url = f"https://github.com/{repository}/releases/download/{tag}/install-openwrt-from-github.sh"
    return (
        '/bin/sh -c \'umask 077; ('
        f'work=$(mktemp -d /tmp/cowplay-install.XXXXXX) && '
        f'curl --fail --location --proto "=https" --proto-redir "=https" --connect-timeout 15 --max-time 180 --max-filesize 16384 "{url}" -o "$work/install.sh" && '
        f'echo "{script_hash}  $work/install.sh" | sha256sum -c - && '
        f'/bin/sh "$work/install.sh" {version} {fingerprint} {channel}'
        ') >/tmp/cowplay-install.log 2>&1 </dev/null & echo "Started; read /tmp/cowplay-install.log for progress."\''
    )


def release_notes(manifest, fingerprint, command, channel='dev'):
    return (
        f"# CowPlay {manifest['tag']}\n\n"
        + ('**开发预发布（Pre-release）** · 用于测试与反馈，尚未完成正式验收。\n\n' if channel == 'dev' else '**正式版**\n\n')
        +
        '无线 CarPlay 接收端，主要面向 MU300 OpenWrt / 中兴 F50。\n\n'
        '## 安装\n\n'
        '- OpenWrt：安装同版本 cowplay 与 luci-app-cowplay 双 APK。\n'
        + ('- Linux：选择 x86_64 或 ARM64 tar.gz，验证后按包内 README 安装。\n' if command else '')
        + f"- [安装与恢复说明](https://github.com/{manifest['repository']}/blob/{manifest['tag']}/docs/RELEASES.md)。\n\n"
        f"OpenWrt 目标平台：{', '.join(manifest['openwrt_releases'])} / {manifest['arch']}。\n\n"
        + ('LuCI 首次安装命令（完整校验后才执行，需管理员授权）：\n\n```sh\n' + command + '\n```\n\n' if command else '')
        + '## 验证与边界\n\n'
        f'发布公钥 PEM 的 SHA-256：`{fingerprint}`。请通过独立可信渠道核对。\n\n'
        '核对公钥后，验证 `SHA256SUMS.sig` 和安装文件 SHA-256；双 APK 及升级清单另有签名。'
        '附件仅含安装与验证材料，许可证和归属随包提供。\n\n'
        '需自行准备合法配件身份、可信 HTTPS 证书和热点。升级保留私有状态，失败保留备份；'
        '不保证仅覆盖旧程序即可回退。真实音频、双手机切换、Ultra／车辆控制、'
        'Linux 完整无线链路及长期稳定性仍需实机验收。\n'
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repository', help='GitHub OWNER/REPO')
    parser.add_argument('--channel', choices=['stable', 'dev'])
    parser.add_argument('--policy', type=Path, help='Release policy instead of explicit repository/compatibility arguments')
    parser.add_argument('--core', type=Path, required=True)
    parser.add_argument('--luci', type=Path, required=True)
    parser.add_argument('--sign-key', type=Path, required=True, help='Existing private release key; never copied into output')
    parser.add_argument('--upgrade-from', action='append', default=[], help='Explicitly tested compatible source version; repeat as needed')
    parser.add_argument('--openwrt-release', action='append', default=[], help='Explicitly tested compatible firmware release; repeat as needed')
    parser.add_argument('--output', type=Path, required=True, help='New directory')
    parser.add_argument('--notes', type=Path, required=True, help='New release body file outside the asset directory')
    parser.add_argument('--apk', default='apk', help='APK v3 executable')
    parser.add_argument('--openssl', default='openssl')
    parser.add_argument('--linux-x86_64', type=Path)
    parser.add_argument('--linux-arm64', type=Path)
    args = parser.parse_args()
    if args.policy:
        if args.repository or args.channel or args.upgrade_from or args.openwrt_release:
            parser.error('--policy cannot be combined with explicit policy values')
        policy = json.loads(args.policy.read_text())
        args.repository = policy['repository']
        args.channel = policy['channel']
        args.upgrade_from = policy['upgrade_from']
        args.openwrt_release = policy['openwrt_releases']
    if args.channel not in ['stable', 'dev']:
        parser.error('explicit channel stable/dev is required (or set it in --policy)')
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
    if args.notes.exists() or args.notes.resolve().is_relative_to(args.output.resolve()):
        parser.error('notes must be a new file outside the asset directory')
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
    bootstrap_binary = None
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
            if arch == 'arm64':
                bootstrap_binary = archive.extractfile(root + '/cowplay').read()
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
        manifest = dict(schema=1, repository=args.repository, tag='v' + (version[:-3] if version.endswith('-r0') else version), version=version, platform='openwrt-apk', openwrt_releases=args.openwrt_release, arch='aarch64_generic', upgrade_from=args.upgrade_from, packages=packages)
        file = args.output / 'cowplay-release.json'
        file.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n')
        signature = args.output / 'cowplay-release.sig'
        run([args.openssl, 'dgst', '-sha256', '-sign', args.sign_key.resolve(), '-out', signature.resolve(), file.resolve()])
        run([args.openssl, 'dgst', '-sha256', '-verify', keys / 'release.pem', '-signature', signature.resolve(), file.resolve()])
        for name, source in linux_files.items():
            shutil.copyfile(source, args.output / name)
        command = None
        if linux_files:
            shutil.copyfile(Path(__file__).resolve().parent / 'install-linux-from-github.sh', args.output / 'install-linux-from-github.sh')
            shutil.copyfile(Path(__file__).resolve().parent / 'install-openwrt-from-github.sh', args.output / 'install-openwrt-from-github.sh')
            (args.output / 'cowplay-updater-aarch64').write_bytes(bootstrap_binary)
            # The administrator pastes this into the official LuCI Custom Commands page.
            # Keep both integrity anchors literal; no downloaded text is evaluated first.
            script_hash = hashlib.sha256((args.output / 'install-openwrt-from-github.sh').read_bytes()).hexdigest()
            fingerprint = hashlib.sha256(public).hexdigest()
            command = bootstrap_command(args.repository, manifest['tag'], version, script_hash, fingerprint, args.channel)
        checksum_file = args.output / 'SHA256SUMS'
        checksums = []
        for file in sorted(args.output.iterdir()):
            with file.open('rb') as stream:
                checksums.append(hashlib.file_digest(stream, 'sha256').hexdigest() + '  ' + file.name)
        checksum_file.write_text('\n'.join(checksums) + '\n')
        checksum_sig = args.output / 'SHA256SUMS.sig'
        run([args.openssl, 'dgst', '-sha256', '-sign', args.sign_key.resolve(), '-out', checksum_sig.resolve(), checksum_file.resolve()])
        run([args.openssl, 'dgst', '-sha256', '-verify', keys / 'release.pem', '-signature', checksum_sig.resolve(), checksum_file.resolve()])
    args.notes.write_text(release_notes(manifest, hashlib.sha256(public).hexdigest(), command, args.channel))
    print(json.dumps(dict(tag=manifest['tag'], output=str(args.output.resolve()), files=sorted(p.name for p in args.output.iterdir()), public_key_sha256=hashlib.sha256(public).hexdigest()), indent=2))


if __name__ == '__main__':
    try:
        main()
    except subprocess.CalledProcessError as error:
        raise SystemExit(f'Local {Path(error.cmd[0]).name} {error.cmd[1]} failed (exit {error.returncode}); incomplete output must not be published\n{error.stdout.decode(errors="replace")[:1000]}{error.stderr.decode(errors="replace")[:1000]}') from None
