#!/usr/bin/env python3
"""Run the real offline firmware restore shell against isolated synthetic tools/state."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

project = Path(__file__).resolve().parent.parent
checks = []


def script(path, body, python=False):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(('#!' + sys.executable + '\n' if python else '#!/bin/sh\n') + body)
    path.chmod(0o700)


for mode in ['enabled', 'disabled', 'already_installed', 'different_version', 'wrong_arch', 'wrong_firmware', 'manifest_signature', 'apk_signature', 'hash', 'dependencies', 'blocked', 'active_update', 'no_cache']:
    with tempfile.TemporaryDirectory(prefix='sysupgrade-restore-', dir=project / 'target') as raw:
        root = Path(raw)
        cache = root / 'etc/cowplay/firmware-restore'
        slot = cache / ('a' * 32)
        slot.mkdir(parents=True)
        binpath = root / 'bin'
        binpath.mkdir()
        (root / 'etc/apk').mkdir(parents=True)
        (root / 'etc/init.d').mkdir(parents=True)
        (root / 'etc/rc.d').mkdir()
        (root / 'etc/cowplay/state').mkdir()
        private = root / 'etc/cowplay/state/identity'
        private.write_text('SYNTHETIC PRIVATE STATE')
        (root / 'etc/apk/arch').write_text('x86_64' if mode == 'wrong_arch' else 'aarch64_generic')
        (root / 'etc/openwrt_release').write_text("DISTRIB_RELEASE='" + ('26.01' if mode == 'wrong_firmware' else '25.12.6') + "'\n")
        (root / 'etc/cowplay/releases.json').write_text('{"repository":"helloworlde/cowplay"}')
        (root / 'etc/cowplay/release-key.pem').write_text('synthetic key')
        (cache / 'current').write_text('a' * 32)
        if mode == 'blocked':
            (cache / 'blocked').write_text('interrupted app update')
        if mode == 'active_update':
            (root / 'etc/cowplay/updates').mkdir()
            (root / 'etc/cowplay/updates/active').touch()
        if mode == 'no_cache':
            (cache / 'current').unlink()
        manifest = dict(schema=1, repository='helloworlde/cowplay', platform='openwrt-apk', openwrt_releases=['25.12.6'], arch='aarch64_generic', version='0.3.14-r17', tag='v0.3.14-r17', packages=[])
        for name, arch in [('cowplay', 'aarch64_generic'), ('luci-app-cowplay', 'noarch')]:
            file = f'{name}-0.3.14-r17-{arch}.apk'
            data = name.encode()
            (slot / file).write_bytes(data)
            manifest['packages'].append(dict(name=name, arch=arch, file=file, sha256=hashlib.sha256(data).hexdigest()))
        if mode == 'hash':
            (slot / manifest['packages'][0]['file']).write_text('tampered')
        (slot / 'cowplay-release.json').write_text(json.dumps(manifest))
        (slot / 'cowplay-release.sig').write_text('synthetic signature')
        # Deliberately stale cached preference; the preserved latest rc.d symlink must win.
        (slot / 'autostart').write_text('disabled' if mode == 'enabled' else 'enabled')
        if mode == 'enabled':
            (root / 'etc/rc.d/S95cowplay').symlink_to('../init.d/cowplay')
        init = root / 'etc/init.d/cowplay'
        init_body = f'''R='{root}'
case "$1" in
 running) exit 1;;
 enable) echo enable >> "$R/events"; ln -sf ../init.d/cowplay "$R/etc/rc.d/S95cowplay";;
 disable) echo disable >> "$R/events"; rm -f "$R/etc/rc.d/S95cowplay";;
 start) echo FORBIDDEN_START >> "$R/events";;
esac
'''
        script(root / 'init-template', init_body)
        installed = {'cowplay': '0.3.14-r17', 'luci-app-cowplay': '0.3.14-r17'} if mode == 'already_installed' else {'cowplay': '0.3.15-r0'} if mode == 'different_version' else {}
        (root / 'installed.json').write_text(json.dumps(installed))
        script(binpath / 'jsonfilter', '''import json,re,sys
args=sys.argv[1:]
value=json.load(open(args[args.index('-i')+1]))
for match in re.finditer(r'\\.([a-z_0-9]+)|\\[([0-9]+|\\*)\\]',args[args.index('-e')+1]):
    key,index=match.groups()
    if key is not None:value=value[key]
    elif index!='*':value=value[int(index)]
if isinstance(value,list):print('\\n'.join(str(x) for x in value))
else:print(value)
''', python=True)
        script(binpath / 'openssl', 'exit ' + ('1' if mode == 'manifest_signature' else '0') + '\n')
        script(binpath / 'logger', 'exit 0\n')
        script(binpath / 'sha256sum', '''import hashlib,sys
line=sys.stdin.read().strip();expected,file=line.split('  ',1)
sys.exit(0 if hashlib.sha256(open(file,'rb').read()).hexdigest()==expected else 1)
''', python=True)
        script(binpath / 'apk', f'''import json,sys,shutil,os
from pathlib import Path
root=Path({str(root)!r});mode={mode!r};args=sys.argv[1:]
current=json.loads((root/'installed.json').read_text())
if 'info' in args:
    constraints=args[args.index('--exists')+1:]
    ok=all((current.get(c.split('=',1)[0])==c.split('=',1)[1] if '=' in c else c in current) for c in constraints)
    sys.exit(0 if ok else 1)
if 'verify' in args:sys.exit(1 if mode=='apk_signature' else 0)
if 'add' in args:
    if '--simulate' in args:sys.exit(1 if mode=='dependencies' else 0)
    with (root/'events').open('a') as f:f.write('install\\n')
    (root/'installed.json').write_text(json.dumps({{'cowplay':'0.3.14-r17','luci-app-cowplay':'0.3.14-r17'}}))
    shutil.copyfile(root/'init-template',root/'etc/init.d/cowplay');os.chmod(root/'etc/init.d/cowplay',0o700)
    # Core post-install enables its default; restore must replace it with latest preference.
    (root/'etc/rc.d/S95cowplay').unlink(missing_ok=True)
    (root/'etc/rc.d/S95cowplay').symlink_to('../init.d/cowplay')
    sys.exit(0)
sys.exit(1)
''', python=True)
        source = (project / 'packaging/cowplay-restore.sh').read_text()
        for path in ['/etc/cowplay', '/etc/init.d/cowplay', '/etc/apk/arch', '/etc/openwrt_release', '/etc/rc.d/S95cowplay']:
            source = source.replace(path, str(root / path.removeprefix('/')))
        actual = root / 'restore.sh'
        actual.write_text(source)
        result = subprocess.run(['/bin/sh', actual], env=dict(os.environ, PATH=str(binpath) + ':' + os.environ['PATH']), capture_output=True, text=True, timeout=15)
        events = (root / 'events').read_text() if (root / 'events').exists() else ''
        expected_success = mode in ['enabled', 'disabled', 'already_installed', 'no_cache']
        assert (result.returncode == 0) == expected_success, (mode, result.stderr, (cache / 'result').read_text() if (cache / 'result').exists() else '')
        assert ('install' in events) == (mode in ['enabled', 'disabled']), (mode, events)
        assert 'FORBIDDEN_START' not in events
        if mode in ['enabled', 'disabled']:
            assert (root / 'etc/rc.d/S95cowplay').is_symlink() == (mode == 'enabled'), (mode, events)
        assert private.read_text() == 'SYNTHETIC PRIVATE STATE'
        checks.append(mode)
for same_version in [True, False]:
    with tempfile.TemporaryDirectory(prefix='sysupgrade-guard-', dir=project / 'target') as raw:
        root=Path(raw);cache=root/'etc/cowplay/firmware-restore';slot=cache/('a'*32);slot.mkdir(parents=True)
        (cache/'current').write_text('a'*32);(slot/'cowplay-release.json').write_text('{"version":"0.3.14-r17"}')
        tools=root/'bin';tools.mkdir();script(tools/'jsonfilter',"printf '0.3.14-r17'\n");script(tools/'apk','exit '+('0' if same_version else '1')+'\n')
        guard=(project/'packaging/cowplay.sysupgrade').read_text().replace('/etc/cowplay',str(root/'etc/cowplay')).replace('/var/run/cowplay-network',str(root/'var/run/cowplay-network')).replace('/etc/rc.d/S95cowplay',str(root/'etc/rc.d/S95cowplay'))
        actual=root/'guard.sh';actual.write_text('append() { :; }\n'+guard+'\ncowplay_preserve_guard "$1"\n');listing=root/'backup-list';listing.touch()
        subprocess.run(['/bin/sh',actual,listing],env=dict(os.environ,PATH=str(tools)+':'+os.environ['PATH']),check=True)
        assert (cache/'blocked').exists() != same_version
        if not same_version:assert str(cache/'blocked') in listing.read_text()
checks.append('sysupgrade hook blocks stale cache after external app version change')
keep = (project / 'packaging/cowplay.keep').read_text().splitlines()
for name in ['/etc/cowplay/state/', '/etc/cowplay/firmware-restore/', '/lib/upgrade/keep.d/cowplay', '/usr/libexec/cowplay-restore', '/etc/rc.d/S94cowplay-restore']:
    assert name in keep
assert not any('/lib/apk/db' in p or '/updates/' in p or p == '/usr/bin/cowplay' for p in keep)
print(json.dumps(dict(result='SYSUPGRADE_RESTORE_ISOLATED_TESTS_PASSED', checks=checks), indent=2))
