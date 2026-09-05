import importlib.machinery
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader('glass_config', str(ROOT / 'payload/bin/nethos-compositor-config'))
spec = importlib.util.spec_from_loader(loader.name, loader)
module = importlib.util.module_from_spec(spec)
loader.exec_module(module)

class GlassConfigTest(unittest.TestCase):
    def setUp(self):
        self.base = (ROOT / 'payload/wayfire/wayfire.ini').read_text()

    def test_never_stacks_effects_and_preserves_user_bindings(self):
        result, effect = module.select_effect(self.base, True, True, False)
        self.assertEqual(effect, 'nethos-glass')
        plugins = result.split('plugins = ')[1].splitlines()[0].split()
        self.assertNotIn('blur', plugins)
        self.assertIn('firedecor', plugins)
        self.assertIn('nethos-glass', plugins)
        self.assertIn('binding_terminal = <super> KEY_RETURN', result)
        self.assertEqual(module.select_effect(result, True, True, False)[0], result)

    def test_fallbacks(self):
        for gpu, available, reduced, expected in [(True, False, False, 'blur'),
                (False, True, False, None), (True, True, True, None)]:
            result, effect = module.select_effect(self.base, gpu, available, reduced)
            self.assertEqual(effect, expected)
            self.assertIn('[firedecor]', result)

    def test_reduced_transparency_also_covers_native_chrome(self):
        self.assertIn(r'active_border = \#161B22FF', module.material(self.base, True, True))

    def test_live_palette_does_not_change_plugins(self):
        before, _ = module.select_effect(self.base, True, True, False)
        light = module.material(before, False)
        self.assertEqual(before.split('plugins = ')[1].splitlines()[0], light.split('plugins = ')[1].splitlines()[0])
        self.assertIn(r'active_border = \#F4F7FAC7', light)
        self.assertIn(r'active_title = \#F1F4F8FF', module.material(light, True))
        self.assertEqual(module.material(light, False), light)

if __name__ == '__main__': unittest.main()
