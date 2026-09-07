app_id = 'org.sysmo.SysMo'

desktop_conf = configuration_data()
desktop_conf.set('BINDIR', bindir)
desktop_conf.set('AUTOSTART', '')

desktop_file = configure_file(
          input: app_id + '.desktop.in',
         output: app_id + '.desktop',
  configuration: desktop_conf,
        install: true,
    install_dir: datadir / 'applications',
)

desktop_utils = find_program('desktop-file-validate', required: false)
if desktop_utils.found()
  test('Validate desktop file', desktop_utils, args: [desktop_file])
endif

if get_option('autostart')
  autostart_conf = configuration_data()
  autostart_conf.set('BINDIR', bindir)
  autostart_conf.set('AUTOSTART', 'X-GNOME-Autostart-enabled=true')

  configure_file(
            input: app_id + '.desktop.in',
           output: app_id + '.autostart.desktop',
    configuration: autostart_conf,
          install: true,
      install_dir: home / '.config' / 'autostart',
      install_tag: 'autostart',
  )
  # configure_file cannot rename on install; fix the name up afterwards.
  meson.add_install_script('sh', '-c',
    'mv -f "$DESTDIR' + home / '.config' / 'autostart' / app_id + '.autostart.desktop" ' +
    '"$DESTDIR' + home / '.config' / 'autostart' / app_id + '.desktop"')
endif

install_data(app_id + '.gschema.xml',
  install_dir: datadir / 'glib-2.0' / 'schemas',
)

compile_schemas = find_program('glib-compile-schemas', required: false)
if compile_schemas.found()
  test('Validate schema file', compile_schemas,
       args: ['--strict', '--dry-run', meson.current_source_dir()])
endif

install_data('icons/hicolor/scalable/apps' / app_id + '.svg',
  install_dir: datadir / 'icons' / 'hicolor' / 'scalable' / 'apps',
)
install_data('icons/hicolor/symbolic/apps' / app_id + '-symbolic.svg',
  install_dir: datadir / 'icons' / 'hicolor' / 'symbolic' / 'apps',
)
