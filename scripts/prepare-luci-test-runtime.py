#!/usr/bin/env python3
"""Fetch the pinned official LuCI runtime for synthetic browser tests only."""
import hashlib
import io
from pathlib import Path
import sys
import tarfile
import urllib.request

commit = 'ddccfc9ec628a9e1a3990e9dd1af8968e80a6970'
output = Path(sys.argv[1])
if output.exists():
    raise SystemExit('Choose a new runtime directory; existing evidence is preserved')
with urllib.request.urlopen(f'https://codeload.github.com/openwrt/luci/tar.gz/{commit}', timeout=90) as response:
    data = response.read(32 * 1024 * 1024 + 1)
if len(data) > 32 * 1024 * 1024:
    raise SystemExit('LuCI archive exceeds size limit')
with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
    for entry in archive:
        if not entry.isfile():
            continue
        name = entry.name.split('/', 1)[1]
        resources = 'modules/luci-base/htdocs/luci-static/resources/'
        theme = 'themes/luci-theme-bootstrap/htdocs/luci-static/bootstrap/'
        if name.startswith(resources):
            relative = name[len(resources):]
        elif name.startswith(theme):
            relative = 'themes/' + name[len(theme):]
            if relative == 'themes/cascade.css':
                relative = 'themes/bootstrap.css'
        else:
            continue
        target = output / relative
        if '..' in Path(relative).parts or Path(relative).is_absolute():
            raise SystemExit('Invalid archive member')
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(archive.extractfile(entry).read())
print(f'Official LuCI {commit}; archive SHA256 {hashlib.sha256(data).hexdigest()}')
