# The full path, so that the launcher works when the install's bin folder
# isn't in the session's PATH (e.g. a --prefix=~/.local install).
desktop_conf = configuration_data()
desktop_conf.set('bindir', bindir)
desktop_file = i18n.merge_file(
  input: configure_file(
    input: app_id + '.desktop.in.in',
    output: app_id + '.desktop.in',
    configuration: desktop_conf,
  ),
  output: app_id + '.desktop',
  type: 'desktop',
  po_dir: '../po',
  install: true,
  install_dir: datadir / 'applications',
)

metainfo_file = i18n.merge_file(
  input: app_id + '.metainfo.xml.in',
  output: app_id + '.metainfo.xml',
  po_dir: '../po',
  install: true,
  install_dir: datadir / 'metainfo',
)

install_data(
  app_id + '.gschema.xml',
  install_dir: datadir / 'glib-2.0' / 'schemas',
)

install_data(
  'icons/hicolor/scalable/apps' / app_id + '.svg',
  install_dir: datadir / 'icons/hicolor/scalable/apps',
)
install_data(
  'icons/hicolor/symbolic/apps' / app_id + '-symbolic.svg',
  install_dir: datadir / 'icons/hicolor/symbolic/apps',
)

desktop_validate = find_program('desktop-file-validate', required: false)
if desktop_validate.found()
  test('validate-desktop', desktop_validate, args: [desktop_file])
endif

compile_schemas = find_program('glib-compile-schemas')
test(
  'validate-schema',
  compile_schemas,
  args: ['--strict', '--dry-run', meson.current_source_dir()],
)

# Lets other apps reveal files ("Show in folder") even when Lia Files isn't
# running. Not installed in a Flatpak, which can only export names under
# its own app ID. The file isn't named after the bus name, so it can be
# installed alongside other file managers' (the bus picks one).
if get_option('file-manager-dbus-service')
  dbus_conf = configuration_data()
  dbus_conf.set('bindir', bindir)
  configure_file(
    input: app_id + '.FileManager1.service.in',
    output: app_id + '.FileManager1.service',
    configuration: dbus_conf,
    install: true,
    install_dir: datadir / 'dbus-1' / 'services',
  )
endif

appstreamcli = find_program('appstreamcli', required: false)
if appstreamcli.found()
  test(
    'validate-metainfo',
    appstreamcli,
    args: ['validate', '--no-net', '--explain', metainfo_file],
  )
endif
