cargo_env = environment()
cargo_env.set('LIA_LOCALEDIR', localedir)

custom_target(
  'cargo-build',
  build_by_default: true,
  build_always_stale: true,
  console: true,
  output: meson.project_name(),
  env: cargo_env,
  install: true,
  install_dir: bindir,
  command: [
    find_program(meson.project_source_root() / 'build-aux/cargo-build.sh'),
    meson.project_source_root() / 'Cargo.toml',
    meson.project_build_root() / 'target',
    get_option('profile'),
    '@OUTPUT@',
  ],
)

test(
  'cargo-test',
  find_program('cargo'),
  args: [
    'test',
    '--manifest-path', meson.project_source_root() / 'Cargo.toml',
    '--target-dir', meson.project_build_root() / 'target',
  ],
  env: cargo_env,
  timeout: 600,
)
