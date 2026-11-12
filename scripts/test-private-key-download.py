import base64
import json
import os
from pathlib import Path
import subprocess
import shlex
import sys
import tempfile

project = Path(__file__).resolve().parents[1]
source = project / 'luci-app-cowplay/htdocs/cgi-bin/cowplay-key-download'
with tempfile.TemporaryDirectory(prefix='cowplay-key-cgi-') as tmp:
    root = Path(tmp)
    secret = b'SYNTHETIC-PRIVATE-KEY-ONLY\x00DER'
    for name, code in {
        'ubus': 'import os,json,sys; request=json.loads(sys.argv[4]) if len(sys.argv)==5 else {}; expected={"ubus_rpc_session":"a"*32,"scope":"ubus","object":"cowplay","function":"private_key_export"}; print(json.dumps({"access":os.getenv("TEST_ACL")=="write" and sys.argv[1:4]==["call","session","access"] and request==expected}))',
        'jsonfilter': 'import sys,json; v=json.load(sys.stdin).get(sys.argv[-1][2:]); print(str(v).lower() if isinstance(v,bool) else v or "")',
        'cowplay': 'import os,json; print(json.dumps({"data":os.environ["TEST_KEY"]}))',
    }.items():
        p = root/name
        p.write_text('#!/usr/bin/env python3\n'+code+'\n')
        p.chmod(0o700)
    script = root/'download'
    script.write_text(source.read_text().replace('PATH=/usr/sbin:/usr/bin:/sbin:/bin', 'PATH='+shlex.quote(str(root)+':'+os.environ['PATH'])).replace('/usr/bin/cowplay', str(root/'cowplay')))
    # LuCI cookies use Path=/cgi-bin/luci and are absent at the sibling CGI URL.
    base = dict(os.environ, REQUEST_METHOD='POST', HTTPS='on', REMOTE_ADDR='192.0.2.1', HTTP_HOST='router.test', HTTP_ORIGIN='https://router.test', CONTENT_TYPE='application/x-www-form-urlencoded', HTTP_COOKIE='', TEST_ACL='write', TEST_KEY=base64.b64encode(secret).decode())
    body = b'kind=tls&sessionid=' + b'a'*32
    def request(body=body, **change):
        env = dict(base, CONTENT_LENGTH=str(len(body)))
        env.update(change)
        return subprocess.run(['sh',str(script)], input=body, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)
    for scheme in ['https', 'http']:
        for kind, name in [(b'tls',b'cowplay-tls-private.pem'), (b'carplay',b'cowplay-carplay-private.pk8')]:
            r=request(b'kind='+kind+b'&sessionid='+b'a'*32, HTTPS='on' if scheme=='https' else '', HTTP_ORIGIN=scheme+'://router.test')
            assert b'Content-Disposition: attachment' in r.stdout, (scheme,r.stdout,r.stderr)
            assert name in r.stdout and r.stdout.endswith(secret)
            assert b'Cache-Control: no-store' in r.stdout and not r.stderr
    rejected = [
        ({'REQUEST_METHOD':'GET'}, body),
        ({'TEST_ACL':'read'}, body),
        ({'HTTP_ORIGIN':'https://evil.test'}, body),
        ({'HTTP_ORIGIN':''}, body),
        ({'HTTPS':'','HTTP_ORIGIN':'https://router.test','HTTP_X_FORWARDED_PROTO':'https'}, body),
        ({'HTTPS':'','HTTP_X_FORWARDED_FOR':'127.0.0.1'}, body),
        ({}, b'kind=tls&sessionid=invalid-secret'),
        ({}, b'kind=tls&sessionid='+b'b'*32),
        ({'HTTP_COOKIE':'sysauth_https='+'a'*32}, b'kind=tls'),
        ({'CONTENT_LENGTH':'9'}, body),
        ({'CONTENT_LENGTH':'400'}, body),
        ({'CONTENT_TYPE':'application/json'}, body),
        ({}, body+b'&kind=carplay'),
        ({}, body+b'&sessionid='+b'a'*32),
        ({}, b'kind=/etc/private&sessionid='+b'a'*32),
    ]
    for change, body in rejected:
        r=request(body, **change)
        assert b'Status: 4' in r.stdout and secret not in r.stdout+r.stderr, (change,r.stdout,r.stderr)
    r=request(HTTPS='',REMOTE_ADDR='127.0.0.1',HTTP_HOST='localhost:18880',HTTP_ORIGIN='http://localhost:18880')
    assert r.stdout.endswith(secret), r.stdout
    r=request(HTTP_COOKIE='sysauth_http='+'b'*32+'; sysauth_https='+'c'*32)
    assert r.stdout.endswith(secret), 'the POST session is authoritative; unrelated cookie paths do not choose a session'
    print('PASS: HTTP/HTTPS TLS/CarPlay attachments, no-store, origin/ACL rejection cases and loopback; synthetic keys only')

    # Exercise the real packager's staging step: invoking `sh CGI` above bypasses
    # the executable bit required by uhttpd. Only the final APK encoder is a stub.
    apk = root/'apk-staging-capture'
    apk.write_text('#!'+sys.executable+'\n'
                   'import shutil,sys\n'
                   'assert sys.argv[1] == "mkpkg"\n'
                   'shutil.copytree(sys.argv[sys.argv.index("--files")+1], '
                   'sys.argv[sys.argv.index("--output")+1])\n')
    apk.chmod(0o700)
    staged = root/'package-stage'
    subprocess.run(['sh', str(project/'scripts/build-luci-apk.sh'), str(staged)],
                   env=dict(os.environ, APK_TOOL=str(apk)), check=True)
    for relative in ['www/cgi-bin/cowplay-key-download', 'usr/libexec/rpcd/cowplay',
                     'etc/uci-defaults/90-cowplay']:
        p = staged/relative
        assert p.stat().st_mode & 0o777 == 0o755, f'packaged entry point is not executable: {relative}'
    assert (staged/'www/luci-static/resources/view/cowplay/main.js').stat().st_mode & 0o777 == 0o644
    staged_cgi = staged/'www/cgi-bin/cowplay-key-download'
    assert staged_cgi.read_bytes() == source.read_bytes(), 'packaging must preserve CGI contents'
    r = subprocess.run([str(staged_cgi)], env=dict(os.environ, REQUEST_METHOD='GET'),
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)
    assert r.stdout.startswith(b'Status: 405 Method Not Allowed\r\n') and not r.stderr
    print('PASS: real LuCI packaging keeps CGI/rpcd/defaults executable, static JS non-executable; staged CGI executes directly and rejects GET')
