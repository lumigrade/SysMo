if get_option('buildtype') == 'debug'
  cargo_profile = 'debug'
  cargo_profile_flag = ''
else
  cargo_profile = 'release'
  cargo_profile_flag = '--release'
endif

source_root = meson.project_source_root()

# The engine: Mission Center's magpie, built unmodified from the subproject.
# Both cargo target directories live in the source tree so that plain
# `cargo build` invocations and this build share their caches.
engine_manifest = source_root / 'subprojects' / 'magpie' / 'Cargo.toml'
engine_target_dir = source_root / 'subprojects' / 'magpie' / 'target'

engine = custom_target('sysmo-magpie',
  build_by_default: true,
  build_always_stale: true,
  output: 'sysmo-magpie',
  console: true,
  install: true,
  install_dir: get_option('bindir'),
  command: [
    'sh', '-c',
    'env MC_PATCH_BINARY=' + patch_bin.full_path()
    + ' ' + cargo.full_path() + ' build ' + cargo_profile_flag
    + ' --manifest-path ' + engine_manifest
    + ' --target-dir ' + engine_target_dir
    + ' && cp ' + engine_target_dir / cargo_profile / 'magpie' + ' @OUTPUT@'
  ],
)

# The frontend.
frontend_manifest = source_root / 'Cargo.toml'
frontend_target_dir = source_root / 'target'

frontend = custom_target('sysmo',
  build_by_default: true,
  build_always_stale: true,
  output: 'sysmo',
  console: true,
  install: true,
  install_dir: get_option('bindir'),
  command: [
    'sh', '-c',
    'env SYSMO_PKGDATADIR=' + pkgdatadir
    + ' ' + cargo.full_path() + ' build ' + cargo_profile_flag
    + ' --manifest-path ' + frontend_manifest
    + ' --target-dir ' + frontend_target_dir
    + ' && cp ' + frontend_target_dir / cargo_profile / 'sysmo' + ' @OUTPUT@'
  ],
)

# Hardware database (PCI/USB vendor and model names) used by the engine.
hwdb_dir = source_root / 'subprojects' / 'magpie' / 'platform-linux' / 'hwdb'
hwdb = custom_target('hwdb',
  output: 'hw.db',
  install: true,
  install_dir: pkgdatadir,
  command: [
    python, hwdb_dir / 'generate_hwdb.py', '--output', '@OUTDIR@',
    hwdb_dir / '20-pci-vendor-model.hwdb',
    hwdb_dir / '20-usb-vendor-model.hwdb',
  ],
)
