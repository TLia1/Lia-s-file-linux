project(
  'lia-files',
  version: '1.0.0',
  meson_version: '>= 1.4.0',
  license: 'GPL-3.0-or-later',
)

i18n = import('i18n')
gnome = import('gnome')

app_id = 'io.github.lia.Files'

dependency('gtk4', version: '>= 4.20')
dependency('libadwaita-1', version: '>= 1.8')
dependency('gtksourceview-5', version: '>= 5.10')
dependency('libarchive', version: '>= 3.6')

find_program('cargo')
find_program('blueprint-compiler')
find_program('glib-compile-resources')
find_program('glib-compile-schemas')

prefix = get_option('prefix')
bindir = prefix / get_option('bindir')
datadir = prefix / get_option('datadir')
localedir = prefix / get_option('localedir')

subdir('data')
subdir('po')
subdir('src')

gnome.post_install(
  glib_compile_schemas: true,
  gtk_update_icon_cache: true,
  update_desktop_database: true,
)
