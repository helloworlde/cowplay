#!/usr/bin/env python3
"""Actual signed downloads and updater in an isolated ARM64 OpenWrt root.

Requires a fresh disposable OpenWrt root, proc mounted inside it, and signed assets.
All network traffic terminates at a synthetic local HTTPS GitHub server.
"""
import hashlib
import http.server
import importlib.util
import json
import os
from pathlib import Path
import shutil
import ssl
import subprocess
import sys
import tempfile
import threading
import time

root, assets = (Path(p).resolve() for p in sys.argv[1:3])
project = Path(__file__).resolve().parent.parent
assert root != Path('/') and (root / 'etc/openwrt_release').is_file()
manifest = json.loads((assets / 'cowplay-release.json').read_text())
version = manifest['version']
tag = manifest['tag']
fingerprint = hashlib.sha256((assets / 'release-key.pem').read_bytes()).hexdigest()
spec = importlib.util.spec_from_file_location('release', project/'scripts/prepare-github-release.py')
release = importlib.util.module_from_spec(spec); spec.loader.exec_module(release)
script_hash = hashlib.sha256((assets/'install-openwrt-from-github.sh').read_bytes()).hexdigest()
command = release.bootstrap_command('helloworlde/cowplay', tag, manifest['version'], script_hash, fingerprint, 'dev')
calls = []
environment = {k:v for k,v in os.environ.items() if 'proxy' not in k.lower()}
def chroot(*args, check=True, **kwargs):
    return subprocess.run(['chroot', str(root), *map(str,args)], env=environment, check=check, capture_output=True, text=True, **kwargs)
def state_hashes():
    return {str(p.relative_to(root)):hashlib.sha256(p.read_bytes()).hexdigest() for p in (root/'etc/cowplay/state').rglob('*') if p.is_file()}
before = state_hashes()
shutil.copyfile(project/'scripts/install-openwrt-from-github.sh',root/'tmp/install.sh')
(root/'etc/hosts').write_text('127.0.0.1 localhost github.com api.github.com\n')
with tempfile.TemporaryDirectory(prefix='bootstrap-tls-') as directory:
    tls = Path(directory)
    subprocess.run(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-days','1','-keyout',str(tls/'ca.key'),'-out',str(tls/'ca.crt'),'-subj','/CN=CowPlay synthetic CA','-addext','basicConstraints=critical,CA:TRUE'],check=True,capture_output=True)
    subprocess.run(['openssl','req','-new','-newkey','rsa:2048','-nodes','-keyout',str(tls/'key'),'-out',str(tls/'request'),'-subj','/CN=github.com'],check=True,capture_output=True)
    (tls/'extensions').write_text('basicConstraints=critical,CA:FALSE\nsubjectAltName=DNS:github.com,DNS:api.github.com\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n')
    subprocess.run(['openssl','x509','-req','-in',str(tls/'request'),'-CA',str(tls/'ca.crt'),'-CAkey',str(tls/'ca.key'),'-CAcreateserial','-days','1','-extfile',str(tls/'extensions'),'-out',str(tls/'cert')],check=True,capture_output=True)
    # Synthetic trusted CA inside this disposable root; TLS validation remains enabled.
    (root/'etc/ssl/certs').mkdir(parents=True,exist_ok=True)
    (root/'etc/ssl/certs/ca-certificates.crt').write_bytes((tls/'ca.crt').read_bytes())
    cert_link=root/'etc/ssl/cert.pem'
    if cert_link.is_symlink():cert_link.unlink()
    cert_link.write_bytes((tls/'ca.crt').read_bytes())
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            calls.append(self.path)
            if self.path in [f'/repos/helloworlde/cowplay/releases/tags/{tag}','/repos/helloworlde/cowplay/releases/latest']:
                data=json.dumps(dict(draft=False,prerelease=True,tag_name=tag,assets=[dict(name=n,state='uploaded',browser_download_url=f'https://github.com/helloworlde/cowplay/releases/download/{tag}/{n}') for n in ['cowplay-release.json','cowplay-release.sig']])).encode()
            elif self.path.startswith(f'/helloworlde/cowplay/releases/download/{tag}/'):
                file=assets/self.path.rsplit('/',1)[-1]
                if not file.is_file():self.send_error(404);return
                data=file.read_bytes()
            else:self.send_error(404);return
            self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
        def log_message(self,*args):pass
    server=http.server.ThreadingHTTPServer(('127.0.0.1',443),Handler)
    context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);context.load_cert_chain(tls/'cert',tls/'key');server.socket=context.wrap_socket(server.socket,server_side=True)
    thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    try:
        result=chroot('/bin/sh','/tmp/install.sh',version,'0'*64,'dev',check=False,timeout=30)
        assert result.returncode != 0 and not (root/'etc/cowplay/releases.json').exists(), 'bad key must not configure trust'
        signature=(assets/'SHA256SUMS.sig').read_bytes()
        try:
            (assets/'SHA256SUMS.sig').write_bytes(b'synthetic invalid signature')
            result=chroot('/bin/sh','/tmp/install.sh',version,fingerprint,'dev',check=False,timeout=30)
            assert result.returncode != 0 and not (root/'etc/cowplay/releases.json').exists(), 'bad signature must not execute helper'
        finally:(assets/'SHA256SUMS.sig').write_bytes(signature)
        # Execute the exact release command that is pasted into LuCI. Its outer
        # script hash is independent of downloaded data and runs before any helper.
        chroot('/bin/sh','-c',command.replace(script_hash, '0'*64),timeout=5)
        log=root/'tmp/cowplay-install.log'
        for _ in range(100):
            if log.exists() and 'FAILED' in log.read_text():break
            time.sleep(.05)
        assert 'FAILED' in log.read_text() and not (root/'etc/cowplay/releases.json').exists()
        result=chroot('/bin/sh','-c',command,timeout=5)
        assert 'Started' in result.stdout
        end=time.monotonic()+120
        latest=root/'etc/cowplay/updates/latest'
        status=None
        while time.monotonic()<end:
            if latest.is_file():
                job=latest.read_text().strip();file=root/f'etc/cowplay/updates/{job}/job.json'
                if file.is_file():
                    status=json.loads(file.read_text())
                    if status['phase'] in ['complete','failed','interrupted']:break
            time.sleep(.2)
        assert status and status['phase']=='complete',(status,log.read_text()[-3000:])
        assert before==state_hashes(), 'private state changed'
        assert Path(str(root)+status['backup']).is_file(), 'local backup missing'
        rows=json.loads(chroot('/usr/bin/apk','--no-network','query','--installed','--format','json','--fields','name,version','cowplay','luci-app-cowplay').stdout)
        assert {p['version'] for p in rows}=={version} and len(rows)==2
        assert not any('restore' in p.name for p in (root/'etc/rc.d').glob('*cowplay*'))
        # The installed rpcd entry can recover update status without the bootstrap shell.
        reply=json.loads(chroot('/usr/libexec/rpcd/cowplay','call','update_status',input='{}').stdout)
        assert reply['phase']=='complete' and reply['locked'] is False
        print(json.dumps(dict(result='SIGNED_BOOTSTRAP_FIRST_INSTALL_DETACHED_WORKER_AND_LUCI_STATUS_PASSED',checks=['wrong fingerprint preserves trust','invalid signature never executes helper','HTTPS GitHub download with CA validation','signed double package first installation','private state and backup retained','background command survives return','installed rpcd restores status'],requests=len(calls)),indent=2))
    finally:server.shutdown();server.server_close();thread.join()
