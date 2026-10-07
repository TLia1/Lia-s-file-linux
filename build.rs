//! Compiles Blueprint UI files, bundles them with the stylesheet into a
//! GResource, and compiles the GSettings schema so that a plain `cargo run`
//! works without installing anything.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ui_out = out_dir.join("ui");
    std::fs::create_dir_all(&ui_out).unwrap();

    println!("cargo::rerun-if-changed=data/resources");
    println!("cargo::rerun-if-changed=data/io.github.lia.Files.gschema.xml");

    // Blueprint → GtkBuilder XML
    let blueprints: Vec<PathBuf> = std::fs::read_dir("data/resources/ui")
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "blp"))
        .collect();
    let status = Command::new("blueprint-compiler")
        .arg("batch-compile")
        .arg(&ui_out)
        .arg("data/resources/ui")
        .args(&blueprints)
        .status()
        .expect("failed to run blueprint-compiler — is it installed?");
    assert!(status.success(), "blueprint-compiler failed");

    // GResource bundle (looks up files in data/resources first, then OUT_DIR)
    glib_build_tools::compile_resources(
        &[PathBuf::from("data/resources"), out_dir.clone()],
        "data/resources/resources.gresource.xml",
        "lia-files.gresource",
    );

    // GSettings schema, used as a fallback when the app is not installed
    let schema_dir = out_dir.join("schemas");
    std::fs::create_dir_all(&schema_dir).unwrap();
    std::fs::copy(
        "data/io.github.lia.Files.gschema.xml",
        schema_dir.join("io.github.lia.Files.gschema.xml"),
    )
    .unwrap();
    let status = Command::new("glib-compile-schemas")
        .arg("--strict")
        .arg(&schema_dir)
        .status()
        .expect("failed to run glib-compile-schemas");
    assert!(status.success(), "glib-compile-schemas failed");
}
