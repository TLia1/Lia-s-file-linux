//! The public theme inputs must be deterministic and must not inject CSS.
use lia_files::util::appearance::{read_stylesheets, valid_accent};

#[test]
fn stylesheets_follow_filename_order_and_deletions() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("penumbra.css"), ":root { --x: red; }").unwrap();
    std::fs::write(dir.path().join("corners.css"), ":root { --radius: 0; }").unwrap();
    std::fs::write(dir.path().join("ignored.txt"), "invalid CSS").unwrap();
    std::fs::create_dir(dir.path().join("folder.css")).unwrap();
    assert_eq!(
        read_stylesheets(dir.path()).unwrap(),
        ":root { --radius: 0; }\n:root { --x: red; }\n"
    );
    std::fs::remove_file(dir.path().join("penumbra.css")).unwrap();
    assert!(!read_stylesheets(dir.path()).unwrap().contains("--x"));
    assert_eq!(read_stylesheets(&dir.path().join("absent")).unwrap(), "");
}

#[test]
fn accent_only_accepts_six_hex_digits() {
    for value in ["#000000", "#b69AF7", "#ffffff"] {
        assert!(valid_accent(value));
    }
    for value in ["", "red", "#abc", "#ffffff; color: red", "#fffffg", "#ééé"] {
        assert!(!valid_accent(value));
    }
}
