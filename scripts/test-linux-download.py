#!/usr/bin/env python3
"""Exercise the actual downloader with a local TLS GitHub fixture and real signatures."""
import gzip
import hashlib
import http.server
import io
import json
import os
from pathlib import Path
import platform
import shutil
import ssl
import subprocess
import tarfile
import tempfile
import threading

project=Path(__file__).resolve().parent.parent
arch='arm64' if platform.machine() in ['aarch64','arm64'] else 'x86_64'
files='cowplay install.sh cowplay.service config.example.json VERSION ARCH UPGRADE_FROM LICENSE NOTICE.md TESLA-PROTOCOL-LICENSE LIVI-NOTICE README.md'.split()
openssl=shutil.which('openssl');curl=shutil.which('curl')
assert openssl and curl,'OpenSSL and curl are required for release download tests'
checks=[]
with tempfile.TemporaryDirectory(prefix='linux-download-',dir=project/'target') as raw:
 root=Path(raw);version='0.3.14-r17';package=f'cowplay-{version}-linux-{arch}';assets=root/'assets';assets.mkdir()
 key=root/'test-release.key'
 def run(args,**kwargs):return subprocess.run([str(a) for a in args],check=True,capture_output=True,**kwargs).stdout
 run([openssl,'genpkey','-algorithm','RSA','-pkeyopt','rsa_keygen_bits:2048','-out',key])
 public=run([openssl,'pkey','-in',key,'-pubout']);fingerprint=hashlib.sha256(public).hexdigest()
 (assets/'release-key.pem').write_bytes(public)
 tls_key=root/'tls.key';tls_cert=root/'tls.pem'
 run([openssl,'req','-x509','-newkey','rsa:2048','-nodes','-days','1','-keyout',tls_key,'-out',tls_cert,'-subj','/CN=github.com','-addext','subjectAltName=DNS:github.com'])
 requests=[];mode='valid'
 class Handler(http.server.BaseHTTPRequestHandler):
  def do_GET(self):
   requests.append(self.path)
   name=self.path.rsplit('/',1)[-1]
   if not self.path.startswith(f'/helloworlde/cowplay/releases/download/v{version}/') or not (assets/name).is_file():self.send_error(404);return
   data=(assets/name).read_bytes();self.send_response(200)
   self.send_header('Content-Length',str(17000 if mode=='oversized_key' and name=='release-key.pem' else len(data)))
   self.end_headers()
   try:self.wfile.write(data)
   except (BrokenPipeError,ssl.SSLError):pass
  def log_message(self,*args):pass
 server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
 context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);context.load_cert_chain(tls_cert,tls_key);server.socket=context.wrap_socket(server.socket,server_side=True)
 worker=threading.Thread(target=server.serve_forever,daemon=True);worker.start()
 try:
  for mode in ['valid','wrong_key','bad_signature','hash','traversal','link','duplicate_checksum','missing_checksum','bad_ca','oversized_key']:
   marker=root/'executed';marker.unlink(missing_ok=True)
   archive=assets/(package+'.tar.gz')
   with archive.open('wb') as raw_file,gzip.GzipFile(filename='',mode='wb',fileobj=raw_file,mtime=0) as gz,tarfile.open(fileobj=gz,mode='w',format=tarfile.USTAR_FORMAT) as tar:
    entry=tarfile.TarInfo(package+'/');entry.type=tarfile.DIRTYPE;entry.mode=0o755;tar.addfile(entry)
    for name in files:
     entry=tarfile.TarInfo(package+'/'+name);entry.mode=0o755 if name=='install.sh' else 0o644
     data=(f'#!/bin/sh\nprintf executed > "{marker}"\n').encode() if name=='install.sh' else b'synthetic fixture'
     if mode=='link' and name=='cowplay':entry.type=tarfile.SYMTYPE;entry.linkname='/etc/passwd';tar.addfile(entry)
     else:entry.size=len(data);tar.addfile(entry,io.BytesIO(data))
    if mode=='traversal':
     entry=tarfile.TarInfo('../../escape');entry.size=6;tar.addfile(entry,io.BytesIO(b'escape'))
   line=hashlib.sha256(archive.read_bytes()).hexdigest()+'  '+archive.name
   if mode=='duplicate_checksum':text=line+'\n'+line+'\n'
   elif mode=='missing_checksum':text=hashlib.sha256(b'x').hexdigest()+'  other.tar.gz\n'
   else:text=line+'\n'
   (assets/'SHA256SUMS').write_text(text)
   run([openssl,'dgst','-sha256','-sign',key,'-out',assets/'SHA256SUMS.sig',assets/'SHA256SUMS'])
   if mode=='bad_signature':(assets/'SHA256SUMS.sig').write_bytes(b'not a signature')
   if mode=='hash':archive.write_bytes(archive.read_bytes()+b'tampered')
   tools=root/'tools';tools.mkdir(exist_ok=True)
   wrapper=tools/'curl';wrapper.write_text('#!/bin/sh\nexec '+str(curl)+' --noproxy "*" --connect-to github.com:443:127.0.0.1:'+str(server.server_port)+('' if mode=='bad_ca' else ' --cacert "'+str(tls_cert)+'"')+' "$@"\n');wrapper.chmod(0o755)
   uname=tools/'uname';uname.write_text('#!/bin/sh\nif [ "$1" = -s ]; then echo Linux; else echo '+arch+'; fi\n');uname.chmod(0o755)
   result=subprocess.run(['/bin/sh',project/'scripts/install-linux-from-github.sh',version,'0'*64 if mode=='wrong_key' else fingerprint,'--no-systemd'],env=dict(os.environ,PATH=str(tools)+':'+os.environ['PATH']),capture_output=True,text=True,timeout=15)
   assert (result.returncode==0)==(mode=='valid'),(mode,result.stdout,result.stderr)
   assert marker.exists()==(mode=='valid'),mode
   checks.append(mode)
  assert requests and all(f'/releases/download/v{version}/' in path for path in requests)
 finally:server.shutdown();server.server_close();worker.join()
print(json.dumps(dict(result='LINUX_SIGNED_HTTPS_DOWNLOAD_AND_PREEXTRACTION_REJECTION_PASSED',checks=checks),indent=2))
