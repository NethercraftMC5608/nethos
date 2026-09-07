#!/usr/bin/env python3
"""Text fallback using exactly the same online/offline installation backend."""
import json
import os
import subprocess
from app import candidates
from offline import find_image, verify_and_mount

while True:
    print('\nNETHOS installer\n1 Download system\n2 Use offline USB system file\n3 Shell / network setup\n4 Restart')
    choice=input('Choose: ').strip()
    if choice=='4': subprocess.run(['systemctl','reboot']);continue
    if choice=='3': subprocess.run(['/bin/bash']);continue
    if choice not in ('1','2'): continue
    source=['--online']
    try:
        if choice=='2':
            image=find_image()
            if not image: print('No offline system file found.');continue
            source=['--source-root',verify_and_mount(image)]
        devices=candidates(json.loads(subprocess.check_output(['lsblk','-b','-J','-o','NAME,PATH,SIZE,MODEL,TYPE,RO,MOUNTPOINTS'],text=True)))
        for i,d in enumerate(devices): print(f"{i+1}. {d['path']} {int(d['size'])/1024**3:.1f} GB {(d.get('model') or '').strip()}")
        index=int(input('Destination disk number: '))-1
        if not 0<=index<len(devices): continue
        target=devices[index]['path']
        # The backend requires its own typed confirmation, before any writes.
        subprocess.run(['nethos-install','--target',target]+source,env=dict(os.environ,NETHOS_PINNED_PAYLOAD='1'))
    except (OSError,ValueError,subprocess.SubprocessError) as error:
        print('Installation could not proceed:',error)
