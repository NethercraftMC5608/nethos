"""Discover and verify the companion image without writing to its volume."""
import hashlib
import json
from pathlib import Path
import subprocess

NAME = 'nethos-system-x86_64.squashfs'


def find_image():
    locations = [Path('/run/live/medium'), Path('/media'), Path('/mnt')]
    for directory in locations:
        candidate = directory / NAME
        if candidate.is_file(): return candidate
    data = json.loads(subprocess.check_output(['lsblk','-J','-p','-o','PATH,TYPE,FSTYPE,MOUNTPOINTS'],text=True))
    def entries(devices):
        for d in devices:
            yield d
            yield from entries(d.get('children',[]))
    for dev in entries(data.get('blockdevices',[])):
        if dev.get('fstype') not in ('vfat','exfat','ext4','iso9660'): continue
        mounted = [p for p in (dev.get('mountpoints') or []) if p]
        for directory in mounted:
            candidate = Path(directory) / NAME
            if candidate.is_file(): return candidate
        if mounted: continue
        directory=Path('/run/nethos/offline-media') / Path(dev['path']).name
        directory.mkdir(parents=True, exist_ok=True)
        opts='ro,nosuid,nodev,noexec' + (',noload' if dev['fstype']=='ext4' else '')
        result=subprocess.run(['mount','-o',opts,dev['path'],str(directory)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        if result.returncode: continue
        candidate=directory/NAME
        if candidate.is_file(): return candidate
        subprocess.run(['umount',str(directory)],check=False)
    return None


def verify_and_mount(image):
    checksum=Path(str(image)+'.sha256')
    if not checksum.is_file(): raise ValueError('The offline file needs its .sha256 checksum alongside it.')
    fields=checksum.read_text().split()
    expected=fields[0].lower() if fields else ''
    if len(expected)!=64 or any(c not in '0123456789abcdef' for c in expected):
        raise ValueError('Invalid checksum file.')
    digest=hashlib.sha256()
    with image.open('rb') as file:
        for chunk in iter(lambda:file.read(4*1024*1024),b''): digest.update(chunk)
    if digest.hexdigest()!=expected: raise ValueError('Offline file checksum mismatch. Copy the file again.')
    target=Path('/run/nethos/system-source');target.mkdir(parents=True,exist_ok=True)
    subprocess.run(['mount','-t','squashfs','-o','loop,ro',str(image),str(target)],check=True)
    try:
        metadata=json.loads((target/'etc/nethos/system-image.json').read_text())
        if not isinstance(metadata,dict) or metadata.get('architecture')!='amd64':
            raise ValueError('The system file is not x86_64.')
        return str(target)
    except (OSError,ValueError):
        subprocess.run(['umount',str(target)],check=False)
        raise
