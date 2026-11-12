#!/usr/bin/env python3
"""Run the production Linux installer against private filesystem/service fixtures."""
import fcntl
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

project = Path(__file__).resolve().parent.parent
files = 'cowplay install.sh cowplay.service config.example.json VERSION ARCH UPGRADE_FROM LICENSE NOTICE.md TESLA-PROTOCOL-LICENSE LIVI-NOTICE README.md'.split()
checks = []


def executable(path, body, python=False):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(('#!' + sys.executable + '\n' if python else '#!/bin/sh\n') + body)
    path.chmod(0o755)


for case in ['fresh', 'fresh_empty_state', 'upgrade_running_enabled', 'upgrade_running_disabled', 'upgrade_stopped', 'same_version', 'downgrade', 'incompatible', 'custom_unit', 'dropin', 'stop_failure', 'stop_timeout', 'backup_failure', 'start_failure', 'disk_full', 'wrong_arch', 'unmanaged', 'locked', 'manual', 'interrupted', 'query_failure', 'bad_stage_root']:
    with tempfile.TemporaryDirectory(prefix='linux-install-', dir=project / 'target') as raw:
        temp = Path(raw)
        root, package, tools = temp / 'root', temp / 'package', temp / 'tools'
        root.mkdir(); package.mkdir(); tools.mkdir()
        version = '0.3.14-r17'
        (package / 'VERSION').write_text(version + '\n')
        (package / 'ARCH').write_text('arm64' if case == 'wrong_arch' else 'x86_64')
        (package / 'UPGRADE_FROM').write_text('0.3.14-r15\n' if case == 'incompatible' else '0.3.14-r16\n')
        for name in files:
            if not (package / name).exists():
                (package / name).write_text('synthetic package ' + name)
        executable(package / 'cowplay', 'if [ "$1" = init-access ]; then test -d "$2" && touch "$2/.synthetic-init-called"; else test "$1" = verify-image; fi\n')
        installer = (project / 'packaging/linux/install.sh').read_text()
        installer = installer.replace('root=${DESTDIR:-}', 'root=' + repr(str(root)))
        # Keep the actual service path, but put every filesystem target under the fixture.
        installer = installer.replace('if [ -n "$root" ]; then staging=yes; manual=yes; else [ "$(id -u)" -eq 0 ] || { echo \'Run as root, or set DESTDIR for a file-only staging install\' >&2; exit 1; }; fi', 'staging=no')
        (package / 'install.sh').write_text(installer)
        (root / 'run/systemd/system').mkdir(parents=True)
        (root / 'etc/cowplay').mkdir(parents=True)
        (root / 'var/lib/cowplay').mkdir(parents=True)
        config = root / 'etc/cowplay/config.json'
        state = root / 'var/lib/cowplay/pairing'
        config.write_bytes(b'SYNTHETIC CONFIG WITH PRIVATE PLACEHOLDERS')
        state.write_bytes(b'SYNTHETIC PAIRING AND CERTIFICATE PLACEHOLDERS')
        before = (config.read_bytes(), state.read_bytes())
        if case=='fresh_empty_state':state.unlink();state.parent.rmdir()
        binary = root / 'usr/local/bin/cowplay'
        base = root / 'usr/local/lib/cowplay'
        unit = root / 'etc/systemd/system/cowplay.service'
        old = '0.3.15-r0' if case == 'downgrade' else version if case in ['same_version','interrupted'] else '0.3.14-r16'
        upgraded = case not in ['fresh', 'fresh_empty_state', 'wrong_arch', 'unmanaged', 'locked', 'query_failure', 'bad_stage_root']
        if upgraded:
            previous = base / 'releases' / old
            previous.mkdir(parents=True)
            base.chmod(0o755)
            for name in files:shutil.copyfile(package / name, previous / name)
            (previous / 'VERSION').write_text(old + '\n')
            base.joinpath('current').symlink_to('releases/' + old)
            binary.parent.mkdir(parents=True)
            binary.symlink_to('../lib/cowplay/current/cowplay')
            share=root/'usr/local/share';share.mkdir(parents=True)
            (share/'cowplay').symlink_to('../lib/cowplay/current')
            unit.parent.mkdir(parents=True);shutil.copyfile(previous/'cowplay.service',unit)
        if case == 'unmanaged':
            binary.parent.mkdir(parents=True);binary.write_text('unmanaged existing program')
        if case == 'custom_unit':unit.write_text('administrator-owned custom unit')
        service = {'active':case.startswith('upgrade_running') or case in ['stop_failure','stop_timeout','backup_failure','start_failure'], 'boot':'enabled' if case == 'upgrade_running_enabled' else 'disabled'}
        (temp/'service.json').write_text(json.dumps(service))
        executable(tools/'systemctl', f'''import json,sys
from pathlib import Path
p=Path({str(temp)!r});unit=Path({str(unit)!r});case={case!r};args=sys.argv[1:]
s=json.loads((p/'service.json').read_text())
with (p/'events').open('a') as out:out.write(' '.join(args)+'\\n')
if args[0]=='show':
 if case=='query_failure':sys.exit(1)
 field=next(a.split('=',1)[1] for a in args if a.startswith('--property='))
 print({{'LoadState':'loaded' if unit.exists() else 'not-found','FragmentPath':str(unit) if unit.exists() else '', 'DropInPaths':'synthetic-dropin' if case=='dropin' else '', 'ActiveState':'active' if s['active'] else 'inactive', 'MainPID':'99999999' if s['active'] else '0'}}[field])
elif args[0]=='is-enabled':print(s['boot']);sys.exit(0 if s['boot']=='enabled' else 1)
elif args[0]=='is-active':sys.exit(0 if s['active'] else 3)
elif args[0]=='stop':
 if case=='stop_failure':sys.exit(1)
 if case!='stop_timeout':s['active']=False
elif args[0]=='start':
 if case=='start_failure':sys.exit(1)
 s['active']=True
elif args[0]=='daemon-reload':pass
else:raise SystemExit('unexpected service mutation: '+str(args))
(p/'service.json').write_text(json.dumps(s))
''', python=True)
        executable(tools/'uname', 'if [ "$1" = -s ]; then echo Linux; else echo x86_64; fi\n')
        executable(tools/'timeout', 'shift\nexec "$@"\n')
        executable(tools/'flock', 'import fcntl,sys\ntry:fcntl.flock(int(sys.argv[-1]),fcntl.LOCK_EX|fcntl.LOCK_NB)\nexcept BlockingIOError:sys.exit(1)\n', python=True)
        executable(tools/'sleep', 'exit 0\n')
        executable(tools/'sync', 'exit 0\n')
        executable(tools/'df', "printf 'Filesystem 1024-blocks Used Available Capacity Mounted\\nfixture 9999999 0 " + ('1' if case=='disk_full' else '9999999') + " 0%% /\\n'\n")
        if sys.platform == 'darwin':
            executable(tools/'mv', '''import os,shutil,sys
args=sys.argv[1:];paths=[a for a in args if not a.startswith('-')]
if any('T' in a for a in args if a.startswith('-')):os.replace(paths[0],paths[1])
else:shutil.move(paths[0],paths[1])
''',python=True)
        if case=='backup_failure':executable(tools/'tar','exit 1\n')
        if case=='interrupted':
            gate=root/'var/lib/cowplay-installer/recovery-required';gate.parent.mkdir(parents=True);gate.write_text('synthetic interrupted transaction')
        held = None
        if case=='locked':
            lock=root/'var/lib/cowplay-installer/lock';lock.parent.mkdir(parents=True);held=lock.open('w');fcntl.flock(held,fcntl.LOCK_EX|fcntl.LOCK_NB)
        env=dict(os.environ,PATH=str(tools)+':'+os.environ['PATH'])
        env.pop('DESTDIR',None)
        if case=='bad_stage_root':
            (package/'install.sh').write_text((project/'packaging/linux/install.sh').read_text());env['DESTDIR']='/'
        result=subprocess.run(['/bin/sh',package/'install.sh']+(['--no-systemd'] if case=='manual' else []),env=env,capture_output=True,text=True,timeout=20)
        if held:held.close()
        good=case in ['fresh','fresh_empty_state','upgrade_running_enabled','upgrade_running_disabled','upgrade_stopped','same_version','manual']
        assert (result.returncode==0)==good,(case,result.stdout,result.stderr)
        if case=='fresh_empty_state':
            assert config.read_bytes()==before[0] and (state.parent/'.synthetic-init-called').is_file()
        else:assert (config.read_bytes(),state.read_bytes())==before,case
        after=json.loads((temp/'service.json').read_text())
        assert after['boot']==service['boot'],case
        events=(temp/'events').read_text() if (temp/'events').exists() else ''
        assert 'enable cowplay' not in events and 'disable cowplay' not in events,case
        if good:
            assert os.readlink(base/'current')=='releases/'+version
            assert (base.stat().st_mode & 0o777)==0o755,case
            assert after['active']==service['active'],case
            if case in ['fresh','fresh_empty_state','upgrade_stopped','same_version','manual']:assert 'start cowplay' not in events,case
            if case.startswith('upgrade_running'):assert events.index('stop cowplay')<events.index('start cowplay'),case
        elif case=='start_failure':
            assert os.readlink(base/'current')=='releases/'+version
            assert (root/'var/lib/cowplay-installer/recovery-required').exists()
            assert not after['active']
        elif upgraded:
            assert os.readlink(base/'current')=='releases/'+old,case
        if case=='backup_failure':assert after['active'] and 'start cowplay' in events
        if case in ['stop_failure','stop_timeout']:assert 'start cowplay' not in events
        checks.append(case)
print(json.dumps(dict(result='LINUX_INSTALL_UPGRADE_STATE_SERVICE_AND_FAILURE_TESTS_PASSED',checks=checks),indent=2))
