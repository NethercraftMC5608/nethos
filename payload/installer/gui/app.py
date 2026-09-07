#!/usr/bin/env python3
"""Small X11/Tk installer; all rendering is 2D and no browser is started."""
import json
import os
from pathlib import Path
import queue
import subprocess
import threading
from offline import find_image, verify_and_mount
import tkinter as tk
from tkinter import ttk

BG, PANEL, INK, MUTED, ACCENT = '#161b22', '#202731', '#f1f4f8', '#bdc6d1', '#3b6ea5'


def candidates(data):
    def mounted(d):
        return any(d.get('mountpoints') or []) or any(mounted(c) for c in d.get('children', []))
    return [d for d in data.get('blockdevices', []) if d.get('type') == 'disk'
            and not d.get('ro') and not mounted(d) and int(d.get('size') or 0) >= 16 * 1024**3]


class Installer:
    def __init__(self, root):
        self.root, self.events, self.disks, self.busy = root, queue.Queue(), [], False
        self.source_root = None
        root.title('Install NETHOS')
        root.configure(bg=BG)
        root.geometry('900x620')
        if not os.environ.get('NETHOS_INSTALLER_PREVIEW'):
            root.attributes('-fullscreen', True)
        root.option_add('*Font', ('DejaVu Sans', 12))
        style = ttk.Style(root)
        style.theme_use('clam')
        style.configure('.', background=BG, foreground=INK, fieldbackground=PANEL, borderwidth=0)
        style.configure('TButton', background=PANEL, foreground=INK, padding=(16, 10))
        style.map('TButton', background=[('active', ACCENT)], foreground=[('disabled', '#708090')])
        style.configure('TEntry', fieldbackground=PANEL, foreground=INK, padding=8)
        style.configure('TCombobox', fieldbackground=PANEL, foreground=INK, padding=8)
        style.map('TCombobox', fieldbackground=[('readonly', PANEL)], foreground=[('readonly', INK)])
        outer = tk.Frame(root, bg=BG, padx=40, pady=32)
        outer.pack(fill='both', expand=True)
        tk.Label(outer, text='NETHOS', fg=MUTED, bg=BG, font=('DejaVu Sans', 11)).pack(anchor='w')
        tk.Label(outer, text='A fresh start.', fg=INK, bg=BG, font=('DejaVu Sans', 26, 'bold')).pack(anchor='w', pady=(12, 8))
        tk.Label(outer, text='A minimal installer. The complete desktop is installed on your disk.', fg=MUTED, bg=BG).pack(anchor='w')
        self.status = tk.StringVar(value='Connect using Ethernet or Wi-Fi. The system packages download during installation.')
        tk.Label(outer, textvariable=self.status, fg=MUTED, bg=BG, wraplength=800, justify='left').pack(anchor='w', pady=(20, 12))
        row = tk.Frame(outer, bg=BG); row.pack(fill='x')
        self.network = ttk.Button(row, text='Connect Wi-Fi', command=self.wifi); self.network.pack(side='left')
        ttk.Button(row, text='Network details', command=lambda:self.command(['nmcli', 'device', 'status'])).pack(side='left', padx=8)
        self.offline_button = ttk.Button(row, text='Find offline USB file', command=self.offline)
        self.offline_button.pack(side='left', padx=8)
        ttk.Button(row, text='Use download', command=self.online).pack(side='left')
        tk.Label(outer, text='Destination disk', fg=INK, bg=BG).pack(anchor='w', pady=(24, 8))
        row = tk.Frame(outer, bg=BG); row.pack(fill='x')
        self.disk = ttk.Combobox(row, state='readonly'); self.disk.pack(side='left', fill='x', expand=True)
        self.refresh = ttk.Button(row, text='Refresh', command=self.scan); self.refresh.pack(side='left', padx=(8, 0))
        self.disk.bind('<<ComboboxSelected>>', self.clear_confirmation)
        self.warning = tk.StringVar(value='Disks in use and disks smaller than 16 GB are not offered.')
        tk.Label(outer, textvariable=self.warning, fg=MUTED, bg=BG).pack(anchor='w', pady=(12, 8))
        self.confirm = ttk.Entry(outer); self.confirm.pack(fill='x')
        self.confirm.bind('<KeyRelease>', self.enable_install)
        row = tk.Frame(outer, bg=BG); row.pack(fill='x', pady=16)
        self.install = ttk.Button(row, text='Erase disk and install', state='disabled', command=self.start); self.install.pack(side='left')
        self.reboot = ttk.Button(row, text='Restart', command=self.restart); self.reboot.pack(side='right')
        self.log = tk.Text(outer, height=8, bg=PANEL, fg=MUTED, relief='flat', font=('DejaVu Sans Mono', 10), padx=12, pady=12, state='disabled')
        self.log.pack(fill='both', expand=True)
        self.scan()
        root.after(100, self.poll)

    def log_line(self, line):
        self.log.configure(state='normal'); self.log.insert('end', line + '\n'); self.log.see('end'); self.log.configure(state='disabled')

    def command(self, args):
        def worker():
            try:
                result = subprocess.run(args, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=40)
                self.events.put(('log', result.stdout.strip()))
            except (OSError, subprocess.SubprocessError) as e:
                self.events.put(('log', str(e)))
        threading.Thread(target=worker, daemon=True).start()

    def scan(self):
        if self.busy: return
        try:
            data = json.loads(subprocess.check_output(['lsblk', '-b', '-J', '-o', 'NAME,PATH,SIZE,MODEL,TYPE,RO,MOUNTPOINTS,MAJ:MIN'], text=True, timeout=4))
            self.disks = candidates(data)
        except (OSError, ValueError, subprocess.SubprocessError):
            self.disks = []
        self.disk['values'] = [f"{d['path']}  ·  {int(d['size'])/1024**3:.1f} GB  ·  {(d.get('model') or 'Disk').strip()}" for d in self.disks]
        if self.disks: self.disk.current(0)
        else: self.disk.set('No unused disk of at least 16 GB found')
        self.clear_confirmation()

    def target(self):
        i = self.disk.current()
        return self.disks[i]['path'] if 0 <= i < len(self.disks) else ''

    def clear_confirmation(self, _=None):
        self.confirm.delete(0, 'end')
        self.warning.set(f"All data on {self.target()} will be erased. Type ERASE {self.target()} to confirm." if self.target() else 'No eligible destination disk.')
        self.enable_install()

    def enable_install(self, _=None):
        allowed = bool(self.target()) and self.confirm.get() == 'ERASE ' + self.target() and not self.busy
        self.install.configure(state='normal' if allowed else 'disabled')

    def start(self):
        target = self.target()
        if self.busy or not target or self.confirm.get() != 'ERASE ' + target: return
        if os.environ.get('NETHOS_INSTALLER_PREVIEW'):
            self.log_line('Preview: disk operations are disabled.'); return
        self.busy = True
        for widget in (self.disk, self.refresh, self.install, self.confirm, self.reboot, self.network, self.offline_button): widget.configure(state='disabled')
        self.status.set(('Copying the offline system.' if self.source_root else 'Downloading and installing the complete system.') + ' Keep the machine powered on.')
        def worker():
            env = dict(os.environ, NETHOS_PINNED_PAYLOAD='1')
            try:
                source = ['--source-root', self.source_root] if self.source_root else ['--online']
                process = subprocess.Popen(['nethos-install', '--no-confirm', '--target', target] + source, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, env=env)
                with open('/var/log/nethos-install.log', 'w') as log:
                    for line in process.stdout:
                        log.write(line); log.flush(); self.events.put(('log', line.rstrip()))
                self.events.put(('done', process.wait()))
            except OSError as e:
                self.events.put(('log', str(e))); self.events.put(('done', 1))
        threading.Thread(target=worker, daemon=True).start()

    def poll(self):
        for _ in range(100):
            try: kind, value = self.events.get_nowait()
            except queue.Empty: break
            if kind == 'log': self.log_line(value)
            elif kind == 'source':
                self.source_root = value
                self.busy = False
                self.offline_button.configure(state='normal')
                self.status.set('Offline system verified. No network is needed for installation.')
                self.scan()
            elif kind == 'offline-error':
                self.busy = False
                self.offline_button.configure(state='normal')
                self.status.set(str(value))
                self.enable_install()
            elif kind == 'done':
                self.busy = False
                self.reboot.configure(state='normal')
                self.status.set('Installed. Remove the installer, then restart to test the full desktop.' if value == 0 else 'Installation stopped. The disk may be partially written. Details are below and in /var/log/nethos-install.log.')
        self.root.after(100, self.poll)

    def online(self):
        if self.busy: return
        self.source_root = None
        self.status.set('Online installation selected. The complete system will download during installation.')
        self.clear_confirmation()

    def offline(self):
        if self.busy: return
        self.busy = True
        self.enable_install()
        self.offline_button.configure(state='disabled')
        self.status.set('Looking for nethos-system-x86_64.squashfs and verifying its checksum…')
        def worker():
            try:
                image = find_image()
                if image is None: raise ValueError('No offline file found. Put the system .squashfs and .sha256 files at the top of a FAT, exFAT or ext4 USB volume.')
                self.events.put(('source', verify_and_mount(image)))
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                self.events.put(('offline-error', str(error)))
        threading.Thread(target=worker, daemon=True).start()

    def wifi(self):
        if self.busy: return
        dialog = tk.Toplevel(self.root); dialog.title('Connect Wi-Fi'); dialog.configure(bg=BG)
        dialog.transient(self.root); dialog.grab_set()
        fields=[]
        for label, secret in [('Network name (SSID)',False), ('Password',True)]:
            tk.Label(dialog, text=label, bg=BG, fg=INK).pack(anchor='w', padx=24, pady=(16,4))
            field=ttk.Entry(dialog, width=36, show='•' if secret else '');field.pack(padx=24);fields.append(field)
        def connect():
            ssid, password = [f.get() for f in fields]
            if not ssid: return
            args=['nmcli','--wait','30','device','wifi','connect',ssid]
            if password: args += ['password',password]
            self.command(args);dialog.destroy()
        ttk.Button(dialog,text='Connect',command=connect).pack(pady=20)

    def restart(self):
        if not self.busy and not os.environ.get('NETHOS_INSTALLER_PREVIEW'):
            subprocess.run(['systemctl','reboot'], check=False)


if __name__ == '__main__':
    root = tk.Tk(); Installer(root); root.mainloop()
