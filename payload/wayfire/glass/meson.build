project('nethos-glass', 'cpp', version: '1.0.0', default_options: ['cpp_std=c++17', 'warning_level=2'])
wayfire = dependency('wayfire', version: ['>=0.9.0', '<0.10.0'])
shared_module('nethos-glass', ['blur.cpp', 'blur-base.cpp', 'kawase.cpp'],
  dependencies: [wayfire, dependency('wf-config'), dependency('pixman-1'), dependency('glesv2')],
  cpp_args: ['-DWLR_USE_UNSTABLE'],
  override_options: ['b_lundef=false'],
  install: true, install_dir: get_option('libdir') / 'wayfire')
install_data('nethos-glass.xml', install_dir: get_option('datadir') / 'wayfire/metadata')
