project('sysmo', 'c',
          version: '0.1.0',
          license: 'GPL-3.0-or-later',
    meson_version: '>= 0.63.0',
  default_options: ['warning_level=2', 'werror=false'],
)

gnome = import('gnome')

home = run_command('sh', '-c', 'echo "$HOME"', check: true).stdout().strip()

# The Rust toolchain normally lives in ~/.cargo/bin, which is not always on PATH
# for non-interactive shells.
cargo = find_program('cargo', home / '.cargo' / 'bin' / 'cargo')
python = find_program('python3')
patch_bin = find_program('patch')
find_program('cmake')

dependency('gtk4', version: '>= 4.14')
dependency('libadwaita-1', version: '>= 1.5')
# Needed by the engine's NVTOP GPU backend.
dependency('libdrm')
dependency('gbm')
dependency('libudev')

prefix = get_option('prefix')
bindir = prefix / get_option('bindir')
datadir = prefix / get_option('datadir')
pkgdatadir = datadir / meson.project_name()

subdir('data')
subdir('src')
subdir('extension')

gnome.post_install(
     glib_compile_schemas: true,
    gtk_update_icon_cache: true,
  update_desktop_database: true,
)
