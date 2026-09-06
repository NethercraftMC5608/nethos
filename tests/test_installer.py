import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

GUI=Path(__file__).resolve().parents[1]/'payload/installer/gui'
sys.path.insert(0,str(GUI))
import offline
# Candidate selection has no display dependency beyond importing Tk.
try:
    from app import candidates
except ImportError:
    candidates=None

class InstallerTests(unittest.TestCase):
    @unittest.skipIf(candidates is None, 'Tk is tested in the Linux builder')
    def test_never_offers_live_or_mounted_or_read_only_disks(self):
        data={'blockdevices':[
            {'path':'/dev/vda','type':'disk','size':32*1024**3,'ro':False,'mountpoints':[None]},
            {'path':'/dev/sda','type':'disk','size':32*1024**3,'ro':False,'children':[{'mountpoints':['/run/live/medium']}]},
            {'path':'/dev/sdb','type':'disk','size':32*1024**3,'ro':True},
            {'path':'/dev/vdb','type':'disk','size':8*1024**3,'ro':False},
            {'path':'/dev/sr0','type':'rom','size':32*1024**3,'ro':False},
        ]}
        self.assertEqual([d['path'] for d in candidates(data)],['/dev/vda'])

    def test_bad_checksum_is_rejected_before_mounting(self):
        with tempfile.TemporaryDirectory() as directory:
            image=Path(directory)/offline.NAME
            image.write_bytes(b'incomplete download')
            Path(str(image)+'.sha256').write_text('0'*64+'  '+offline.NAME)
            with patch.object(offline.subprocess,'run') as run:
                with self.assertRaisesRegex(ValueError,'checksum mismatch'):
                    offline.verify_and_mount(image)
                run.assert_not_called()

    def test_missing_checksum_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError,'checksum'):
                offline.verify_and_mount(Path(directory)/offline.NAME)

if __name__=='__main__':unittest.main()
