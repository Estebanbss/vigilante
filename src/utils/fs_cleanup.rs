use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Result summary for cleanup operation
#[derive(Debug)]
pub struct CleanupSummary {
    pub files_deleted: usize,
    pub bytes_freed: u64,
    pub errors: usize,
}

impl CleanupSummary {
    pub fn new() -> Self {
        CleanupSummary {
            files_deleted: 0,
            bytes_freed: 0,
            errors: 0,
        }
    }
}

/// Walks the given root paths (recursively) and deletes files strictly smaller than `max_size_bytes`.
///
/// - `paths`: iterator of filesystem paths to walk. Each path that is a file will be considered; directories will be walked recursively.
/// - `max_size_bytes`: files with size < `max_size_bytes` will be deleted.
/// - Returns a `CleanupSummary` with counts of deleted files, bytes freed and errors encountered.
///
/// Safety: This function will perform deletes. Use carefully and prefer passing test directories when testing.
pub fn remove_small_files_under_paths<P: AsRef<Path>, I: IntoIterator<Item = P>>(paths: I, max_size_bytes: u64) -> Result<CleanupSummary, io::Error> {
    let mut summary = CleanupSummary::new();

    for root in paths {
        let root = root.as_ref();
        if !root.exists() {
            log::warn!("fs_cleanup: path does not exist: {}", root.display());
            continue;
        }

        if root.is_file() {
            match process_file(root, max_size_bytes) {
                Ok((deleted, freed)) => {
                    if deleted {
                        summary.files_deleted += 1;
                        summary.bytes_freed += freed;
                        log::info!("fs_cleanup: deleted file {} ({} bytes)", root.display(), freed);
                    }
                }
                Err(e) => {
                    summary.errors += 1;
                    log::warn!("fs_cleanup: failed to process file {}: {}", root.display(), e);
                }
            }
            continue;
        }

        // Walk directory recursively using a simple stack to avoid deep recursion
        let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
        while let Some(path) = stack.pop() {
            let read = match fs::read_dir(&path) {
                Ok(rd) => rd,
                Err(e) => {
                    summary.errors += 1;
                    log::warn!("fs_cleanup: failed to read dir {}: {}", path.display(), e);
                    continue;
                }
            };

            for entry in read {
                match entry {
                    Ok(ent) => {
                        let p = ent.path();
                        if p.is_dir() {
                            stack.push(p);
                        } else if p.is_file() {
                            match process_file(&p, max_size_bytes) {
                                Ok((deleted, freed)) => {
                                    if deleted {
                                        summary.files_deleted += 1;
                                        summary.bytes_freed += freed;
                                        log::info!("fs_cleanup: deleted file {} ({} bytes)", p.display(), freed);
                                    }
                                }
                                Err(e) => {
                                    summary.errors += 1;
                                    log::warn!("fs_cleanup: failed to process file {}: {}", p.display(), e);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        summary.errors += 1;
                        log::warn!("fs_cleanup: failed to read dir entry in {}: {}", path.display(), e);
                    }
                }
            }
        }
    }

    Ok(summary)
}

fn process_file(path: &Path, max_size_bytes: u64) -> Result<(bool, u64), io::Error> {
    let meta = fs::metadata(path)?;
    let len = meta.len();
    if len < max_size_bytes {
        // Check if file was modified recently (within last 10 minutes) to avoid deleting active recordings
        let modified = meta.modified()?;
        let now = std::time::SystemTime::now();
        let age = now.duration_since(modified).unwrap_or(std::time::Duration::from_secs(0));
        if age < std::time::Duration::from_secs(600) { // 10 minutes
            log::info!("fs_cleanup: skipping recently modified file {} ({} seconds old)", path.display(), age.as_secs());
            return Ok((false, 0));
        }
        // Attempt delete
        match fs::remove_file(path) {
            Ok(()) => Ok((true, len)),
            Err(e) => Err(e),
        }
    } else {
        Ok((false, 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn deletes_small_files_and_keeps_large() {
        let tmp = TempDir::new().unwrap();
        let small = tmp.path().join("small.bin");
        let large = tmp.path().join("large.bin");

        // small 512 bytes
        let mut f = File::create(&small).unwrap();
        f.write_all(&vec![0u8; 512]).unwrap();
        f.sync_all().unwrap();

        // large 25 MB
        let mut f2 = File::create(&large).unwrap();
        f2.write_all(&vec![0u8; 25 * 1024 * 1024]).unwrap();
        f2.sync_all().unwrap();

        let max = 20 * 1024 * 1024u64; // 20 MB
        let res = remove_small_files_under_paths(vec![tmp.path()], max).unwrap();
        assert_eq!(res.files_deleted, 1);
        assert_eq!(res.errors, 0);
        assert!(res.bytes_freed >= 512);

        assert!(!small.exists());
        assert!(large.exists());
    }
}
