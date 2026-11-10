//! One-pass local disk scan. Never follows links or crosses filesystem boundaries.
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    os::unix::fs::MetadataExt,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub parent: Option<usize>,
    pub directory: bool,
    pub bytes: u64,
}

#[derive(Debug, Default)]
pub struct Scan {
    pub entries: Vec<Entry>,
    pub children: HashMap<usize, Vec<usize>>,
    pub unreadable: u64,
    pub skipped: u64,
}

/// Allocated bytes include directory metadata; hard links are counted once.
/// Reverse aggregation avoids recursion even for deeply nested trees.
pub fn scan(
    root: PathBuf,
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize),
) -> io::Result<Scan> {
    if ["/proc", "/sys", "/dev"]
        .iter()
        .any(|path| root.starts_with(path))
    {
        return Err(io::Error::other(
            "Virtual system folders cannot be analyzed",
        ));
    }
    let metadata = fs::symlink_metadata(&root)?;
    if !metadata.is_dir() {
        return Err(io::Error::other("Choose a local folder to analyze"));
    }
    let device = metadata.dev();
    let mut result = Scan::default();
    result.entries.push(Entry {
        path: root,
        parent: None,
        directory: true,
        bytes: metadata.blocks().saturating_mul(512),
    });
    let mut pending = vec![0];
    let mut seen = HashSet::new();
    let mut seen_directories = HashSet::from([(metadata.dev(), metadata.ino())]);
    while let Some(parent) = pending.pop() {
        if cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "Scan cancelled"));
        }
        let children = match fs::read_dir(&result.entries[parent].path) {
            Ok(children) => children,
            Err(_) => {
                result.unreadable += 1;
                continue;
            }
        };
        for child in children {
            if cancel.load(Ordering::Relaxed) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "Scan cancelled"));
            }
            let child = match child {
                Ok(child) => child,
                Err(_) => {
                    result.unreadable += 1;
                    continue;
                }
            };
            let path = child.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => {
                    result.unreadable += 1;
                    continue;
                }
            };
            if metadata.dev() != device || matches!(path.to_str(), Some("/proc" | "/sys" | "/dev"))
            {
                result.skipped += 1;
                continue;
            }
            let directory = metadata.is_dir();
            if directory && !seen_directories.insert((metadata.dev(), metadata.ino())) {
                result.skipped += 1;
                continue;
            }
            let bytes = if !directory
                && metadata.nlink() > 1
                && !seen.insert((metadata.dev(), metadata.ino()))
            {
                0
            } else {
                metadata.blocks().saturating_mul(512)
            };
            let index = result.entries.len();
            result.entries.push(Entry {
                path,
                parent: Some(parent),
                directory,
                bytes,
            });
            result.children.entry(parent).or_default().push(index);
            if directory {
                pending.push(index);
            }
            progress(result.entries.len());
        }
    }
    for index in (1..result.entries.len()).rev() {
        let entry = &result.entries[index];
        let (parent, bytes) = (entry.parent.unwrap(), entry.bytes);
        result.entries[parent].bytes = result.entries[parent].bytes.saturating_add(bytes);
    }
    for children in result.children.values_mut() {
        children.sort_unstable_by(|&a, &b| {
            result.entries[b]
                .bytes
                .cmp(&result.entries[a].bytes)
                .then_with(|| result.entries[a].path.cmp(&result.entries[b].path))
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_allocated_space_once_and_does_not_follow_links() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("nested")).unwrap();
        fs::write(root.path().join("nested/data"), vec![1; 8192]).unwrap();
        fs::hard_link(
            root.path().join("nested/data"),
            root.path().join("duplicate"),
        )
        .unwrap();
        std::os::unix::fs::symlink(root.path(), root.path().join("loop")).unwrap();
        let scan = scan(root.path().into(), &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(scan.entries.len(), 5);
        let expected: u64 = [
            root.path().to_path_buf(),
            root.path().join("nested"),
            root.path().join("nested/data"),
            root.path().join("loop"),
        ]
        .iter()
        .map(|p| fs::symlink_metadata(p).unwrap().blocks() * 512)
        .sum();
        assert_eq!(scan.entries[0].bytes, expected);
        assert_eq!(scan.unreadable, 0);
    }
    #[test]
    fn sparse_files_hidden_files_and_sorted_children() {
        let root = tempfile::tempdir().unwrap();
        fs::File::create(root.path().join("sparse"))
            .unwrap()
            .set_len(1 << 30)
            .unwrap();
        fs::write(root.path().join(".hidden"), vec![1; 16384]).unwrap();
        fs::create_dir(root.path().join("empty")).unwrap();
        let scan = scan(root.path().into(), &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(scan.entries.len(), 4);
        assert!(scan.entries[0].bytes < 1 << 30);
        let children = &scan.children[&0];
        assert!(
            children
                .windows(2)
                .all(|pair| scan.entries[pair[0]].bytes >= scan.entries[pair[1]].bytes)
        );
        assert!(scan.entries.iter().any(|e| e.path.ends_with(".hidden")));
    }

    #[test]
    fn unreadable_folders_are_reported() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let denied = root.path().join("denied");
        fs::create_dir(&denied).unwrap();
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o0)).unwrap();
        let inaccessible = fs::read_dir(&denied).is_err();
        let scan = scan(root.path().into(), &AtomicBool::new(false), |_| {}).unwrap();
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o700)).unwrap();
        if inaccessible {
            assert_eq!(scan.unreadable, 1);
        }
    }

    #[test]
    fn cancellation_and_missing_root() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            scan(root.path().into(), &AtomicBool::new(true), |_| {})
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(scan(root.path().join("missing"), &AtomicBool::new(false), |_| {}).is_err());
    }
}
