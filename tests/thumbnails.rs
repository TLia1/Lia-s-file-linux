//! Thumbnail service: spec paths and validity, thumbnailer files, and the
//! worker's lookup / generate / fail logic against a temporary cache.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gtk::gdk_pixbuf;
use gtk::prelude::*;
use lia_files::services::thumbnails::spec::{self, ThumbSize};
use lia_files::services::thumbnails::thumbnailers::{Registry, Thumbnailer};
use lia_files::services::thumbnails::worker::{self, Job, Outcome};
use lia_files::widgets::details_pane::decode_preview;

#[test]
fn sizes_cover_the_requested_pixels() {
    assert_eq!(ThumbSize::for_pixels(48), ThumbSize::Normal);
    assert_eq!(ThumbSize::for_pixels(128), ThumbSize::Normal);
    assert_eq!(ThumbSize::for_pixels(192), ThumbSize::Large);
    assert_eq!(ThumbSize::for_pixels(320), ThumbSize::XLarge);
    assert_eq!(ThumbSize::for_pixels(5000), ThumbSize::XXLarge);
    let larger: Vec<_> = ThumbSize::Large.and_larger().collect();
    assert_eq!(
        larger,
        [ThumbSize::Large, ThumbSize::XLarge, ThumbSize::XXLarge]
    );
}

#[test]
fn file_names_are_the_md5_of_the_uri() {
    // The example from the thumbnail spec.
    assert_eq!(
        spec::file_name("file:///home/jens/photos/me.png"),
        "c6ee772d9e49320e97ec29a7eb5b1697.png"
    );
    let root = Path::new("/cache/thumbnails");
    assert_eq!(
        spec::thumbnail_path(root, "file:///home/jens/photos/me.png", ThumbSize::Large),
        root.join("large/c6ee772d9e49320e97ec29a7eb5b1697.png")
    );
    assert_eq!(
        spec::fail_path(root, "file:///home/jens/photos/me.png"),
        root.join("fail/lia-files/c6ee772d9e49320e97ec29a7eb5b1697.png")
    );
}

#[test]
fn thumbnailer_files_are_parsed() {
    let data = "[Thumbnailer Entry]\n\
                TryExec=ffmpegthumbnailer\n\
                Exec=ffmpegthumbnailer -i %i -o %o -s %s -f\n\
                MimeType=video/mp4;video/webm;\n";
    let thumbnailer = Thumbnailer::parse(data, false).unwrap();
    assert_eq!(thumbnailer.mime_types, ["video/mp4", "video/webm"]);

    let argv = thumbnailer
        .command(
            Path::new("/videos/my clip.mp4"),
            "file:///videos/my%20clip.mp4",
            Path::new("/tmp/out.png"),
            256,
        )
        .unwrap();
    let argv: Vec<_> = argv.iter().map(|a| a.to_str().unwrap()).collect();
    assert_eq!(
        argv,
        [
            "ffmpegthumbnailer",
            "-i",
            "/videos/my clip.mp4",
            "-o",
            "/tmp/out.png",
            "-s",
            "256",
            "-f"
        ]
    );

    let uri = Thumbnailer {
        exec: "thumb --uri=%u --size %s 100%%".into(),
        mime_types: vec!["a/b".into()],
    };
    let argv = uri
        .command(Path::new("/x"), "file:///x", Path::new("/o"), 128)
        .unwrap();
    let argv: Vec<_> = argv.iter().map(|a| a.to_str().unwrap()).collect();
    assert_eq!(argv, ["thumb", "--uri=file:///x", "--size", "128", "100%"]);

    // Missing programs are skipped; broken files are rejected.
    let missing = "[Thumbnailer Entry]\nTryExec=no-such-program-xyz\nExec=x %i\nMimeType=a/b;\n";
    assert!(Thumbnailer::parse(missing, true).is_none());
    assert!(Thumbnailer::parse("[Thumbnailer Entry]\nExec=x\n", false).is_none());
}

#[test]
fn earlier_directories_take_precedence() {
    let user = tempfile::tempdir().unwrap();
    let system = tempfile::tempdir().unwrap();
    let write = |dir: &Path, name: &str, exec: &str| {
        let data = format!("[Thumbnailer Entry]\nExec={exec}\nMimeType=video/mp4;image/png;\n");
        std::fs::write(dir.join(name), data).unwrap();
    };
    write(user.path(), "mine.thumbnailer", "mine %i %o");
    write(system.path(), "system.thumbnailer", "system %i %o");
    std::fs::write(system.path().join("ignored.txt"), "not a thumbnailer").unwrap();

    let registry = Registry::from_dirs(&[user.path().to_owned(), system.path().to_owned()], false);
    assert_eq!(registry.find("video/mp4").unwrap().exec, "mine %i %o");
    assert!(registry.find("text/plain").is_none());
}

/// A temporary cache, a test image, and a job for it.
struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    image: PathBuf,
    uri: String,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("thumbnails");
        let image = dir.path().join("photo.png");
        let pixbuf =
            gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, 400, 200).unwrap();
        pixbuf.fill(0x3366ccff);
        pixbuf.savev(&image, "png", &[]).unwrap();
        let uri = gtk::gio::File::for_path(&image).uri().to_string();
        Self {
            _dir: dir,
            root,
            image,
            uri,
        }
    }

    fn job(&self, mime_type: &str, mtime: i64, size: ThumbSize) -> Job {
        Job {
            uri: self.uri.clone(),
            path: Some(self.image.clone()),
            mime_type: mime_type.into(),
            mtime,
            size,
            generate: true,
            cancelled: Arc::new(AtomicBool::new(false)),
            reply: async_channel::bounded(1).0,
        }
    }
}

fn ready_size(outcome: Outcome) -> (i32, i32) {
    match outcome {
        Outcome::Ready(image) => (image.width, image.height),
        other => panic!("expected a thumbnail, got {other:?}"),
    }
}

#[test]
fn thumbnails_are_made_saved_and_reused() {
    let fixture = Fixture::new();
    let copy = Thumbnailer {
        exec: "cp %i %o".into(),
        mime_types: vec!["image/png".into()],
    };
    let mut registry = Registry::default();
    registry.add(copy);

    // Made by the thumbnailer, scaled down to fit, and saved with its keys.
    let job = fixture.job("image/png", 1000, ThumbSize::Normal);
    assert_eq!(
        ready_size(worker::process(&job, &fixture.root, &registry)),
        (128, 64)
    );
    let saved = spec::thumbnail_path(&fixture.root, &fixture.uri, ThumbSize::Normal);
    assert!(spec::load_valid(&saved, &fixture.uri, 1000).is_some());
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&saved).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);

    // Found in the cache: no thumbnailer needed.
    let empty = Registry::default();
    assert_eq!(
        ready_size(worker::process(&job, &fixture.root, &empty)),
        (128, 64)
    );

    // A larger size isn't served from the smaller one…
    let large = fixture.job("image/png", 1000, ThumbSize::Large);
    assert_eq!(
        ready_size(worker::process(&large, &fixture.root, &registry)),
        (256, 128)
    );
    // …but a smaller one is served from a larger one.
    std::fs::remove_file(&saved).unwrap();
    assert_eq!(
        ready_size(worker::process(&job, &fixture.root, &empty)),
        (256, 128)
    );

    // Once the file changes, the old thumbnail is stale.
    assert!(spec::load_valid(&saved, &fixture.uri, 2000).is_none());
    let mut changed = fixture.job("video/mp4", 2000, ThumbSize::Large);
    changed.generate = false;
    assert!(matches!(
        worker::process(&changed, &fixture.root, &empty),
        Outcome::Failed
    ));
    // Without a thumbnailer for the type, it's unsupported.
    changed.generate = true;
    assert!(matches!(
        worker::process(&changed, &fixture.root, &empty),
        Outcome::Unsupported
    ));
}

#[test]
fn failures_are_recorded_until_the_file_changes() {
    let fixture = Fixture::new();
    let mut failing = Registry::default();
    failing.add(Thumbnailer {
        exec: "false %i %o".into(),
        mime_types: vec!["video/mp4".into()],
    });
    let job = fixture.job("video/mp4", 1000, ThumbSize::Normal);
    assert!(matches!(
        worker::process(&job, &fixture.root, &failing),
        Outcome::Failed
    ));
    let fail = spec::fail_path(&fixture.root, &fixture.uri);
    assert!(spec::load_valid(&fail, &fixture.uri, 1000).is_some());

    // Not retried, even with a working thumbnailer…
    let mut working = Registry::default();
    working.add(Thumbnailer {
        exec: "cp %i %o".into(),
        mime_types: vec!["video/mp4".into()],
    });
    assert!(matches!(
        worker::process(&job, &fixture.root, &working),
        Outcome::Failed
    ));
    // …until the file changes.
    let changed = fixture.job("video/mp4", 1001, ThumbSize::Normal);
    assert!(matches!(
        worker::process(&changed, &fixture.root, &working),
        Outcome::Ready(_)
    ));
}

#[test]
fn images_without_a_thumbnailer_are_loaded_in_process() {
    let fixture = Fixture::new();
    let job = fixture.job("image/png", 1000, ThumbSize::Normal);
    assert_eq!(
        ready_size(worker::process(&job, &fixture.root, &Registry::default())),
        (128, 64)
    );
}

#[test]
fn nothing_is_made_when_generation_is_off() {
    let fixture = Fixture::new();
    let mut job = fixture.job("image/png", 1000, ThumbSize::Normal);
    job.generate = false;
    assert!(matches!(
        worker::process(&job, &fixture.root, &Registry::default()),
        Outcome::Failed
    ));
    // Not recorded as a failure: another policy may allow it.
    assert!(!spec::fail_path(&fixture.root, &fixture.uri).exists());
}

#[test]
fn text_previews_are_decoded() {
    assert_eq!(decode_preview(b"hello\n").as_deref(), Some("hello\n"));
    // Binary data isn't text.
    assert_eq!(decode_preview(b"\x89PNG\x00\x01"), None);
    assert_eq!(decode_preview(b""), None);
    // A character cut by the size limit is dropped.
    let cut = &"café".as_bytes()[..4];
    assert_eq!(decode_preview(cut).as_deref(), Some("caf"));
    // Latin-1 is shown with replacement characters.
    assert_eq!(decode_preview(b"caf\xe9!").as_deref(), Some("caf\u{fffd}!"));
}
