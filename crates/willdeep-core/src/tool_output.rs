//! Private, content-addressed tool output pages used by context compaction.

use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

mod directory;
use directory::Directory;

const PAGE_CHARACTERS: usize = 8_000;
const MAX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct ToolOutputStore {
    directory: PathBuf,
}

impl ToolOutputStore {
    pub fn new(root: &Path, workspace: &Path) -> Self {
        let scope = format!(
            "{:x}",
            Sha256::digest(workspace.to_string_lossy().as_bytes())
        );
        Self {
            directory: root.join(scope),
        }
    }

    pub fn save(&self, text: &str) -> std::io::Result<String> {
        if text.len() as u64 > MAX_OUTPUT_BYTES {
            return Err(std::io::Error::other("tool output exceeds archive limit"));
        }
        let directory = Directory::open(&self.directory, true)?;
        let id = format!("{:x}", Sha256::digest(text.as_bytes()));
        let temporary = format!(".pending-{}", uuid::Uuid::new_v4());
        let mut file = directory.file(&temporary, true)?;
        let result = (|| {
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            // Publish only complete bytes. A competing writer must never see
            // a partially written file at the content-addressed final name.
            match directory.publish(&temporary, &id) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Self::read_from(&directory, &id)? == text {
                        Ok(())
                    } else {
                        Err(std::io::Error::other(
                            "tool output archive content mismatch",
                        ))
                    }
                }
                Err(error) => Err(error),
            }
        })();
        drop(file);
        let cleanup = directory.remove(&temporary);
        result?;
        cleanup?;
        directory.sync()?;
        Ok(id)
    }

    fn read(&self, id: &str) -> std::io::Result<String> {
        if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(std::io::Error::other("invalid tool output id"));
        }
        let directory = Directory::open(&self.directory, false)?;
        Self::read_from(&directory, id)
    }

    fn read_from(directory: &Directory, id: &str) -> std::io::Result<String> {
        let file = directory.file(id, false)?;
        let mut text = String::new();
        file.take(MAX_OUTPUT_BYTES + 1).read_to_string(&mut text)?;
        if text.len() as u64 > MAX_OUTPUT_BYTES
            || format!("{:x}", Sha256::digest(text.as_bytes())) != id
        {
            return Err(std::io::Error::other(
                "tool output archive integrity check failed",
            ));
        }
        Ok(text)
    }

    pub fn page(&self, id: &str, offset: usize, limit: usize) -> std::io::Result<String> {
        let text = self.read(id)?;
        let length = text.chars().count();
        let limit = limit.clamp(1, PAGE_CHARACTERS);
        let page: String = text.chars().skip(offset).take(limit).collect();
        Ok(format!(
            "tool_output={id}; characters {offset}..{} of {length}\n{page}",
            offset.saturating_add(page.chars().count())
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn user_controlled_ancestor_links_cannot_redirect_archive_creation() {
        let root = std::env::temp_dir().join(format!("output-ancestor-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("outside")).unwrap();
        std::os::unix::fs::symlink(root.join("outside"), root.join("alias")).unwrap();
        let store = ToolOutputStore::new(&root.join("alias/archive"), Path::new("workspace"));
        assert!(store.save("must not escape").is_err());
        assert_eq!(std::fs::read_dir(root.join("outside")).unwrap().count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn replaced_scope_cannot_redirect_open_handles_or_new_reads() {
        let root = std::env::temp_dir().join(format!("output-scope-{}", uuid::Uuid::new_v4()));
        let store = ToolOutputStore::new(&root, Path::new("workspace"));
        let other = ToolOutputStore::new(&root, Path::new("other"));
        let id = store.save("original").unwrap();
        let other_id = other.save("other workspace").unwrap();
        let opened = Directory::open(&store.directory, false).unwrap();
        let original_directory = root.join("moved");
        std::fs::rename(&store.directory, &original_directory).unwrap();
        std::os::unix::fs::symlink(&other.directory, &store.directory).unwrap();
        assert!(store.page(&other_id, 0, 100).is_err());
        assert!(store.save("must not redirect").is_err());
        assert_eq!(
            ToolOutputStore::read_from(&opened, &id).unwrap(),
            "original"
        );
        opened
            .file("temporary", true)
            .unwrap()
            .write_all(b"anchored")
            .unwrap();
        opened.publish("temporary", "published").unwrap();
        opened.remove("temporary").unwrap();
        assert_eq!(
            std::fs::read(original_directory.join("published")).unwrap(),
            b"anchored"
        );
        assert!(!other.directory.join("published").exists());
        assert!(!other.directory.join("temporary").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_publication_never_exposes_partial_content() {
        let root = std::env::temp_dir().join(format!("output-concurrent-{}", uuid::Uuid::new_v4()));
        let store = ToolOutputStore::new(&root, Path::new("workspace"));
        let text = "并发内容".repeat(100_000);
        let barrier = std::sync::Barrier::new(4);
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        let id = store.save(&text).unwrap();
                        assert_eq!(store.read(&id).unwrap(), text);
                    })
                })
                .collect();
            for worker in workers {
                worker.join().unwrap();
            }
        });
        assert_eq!(std::fs::read_dir(&store.directory).unwrap().count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_special_files_are_refused_without_opening_them() {
        use std::os::unix::ffi::OsStrExt;
        let root = std::env::temp_dir().join(format!("output-special-{}", uuid::Uuid::new_v4()));
        let store = ToolOutputStore::new(&root, Path::new("workspace"));
        let id = store.save("content").unwrap();
        let path = store.directory.join(&id);
        std::fs::remove_file(&path).unwrap();
        let target = root.join("elsewhere");
        std::fs::write(&target, "content").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(store.read(&id).is_err());
        std::fs::remove_file(&path).unwrap();
        let fifo = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: The path is NUL-terminated and points into this test's directory.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(store.read(&id).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persisted_unicode_pages_are_retrievable_after_reopening() {
        let root = std::env::temp_dir().join(format!("output-pages-{}", uuid::Uuid::new_v4()));
        let store = ToolOutputStore::new(&root, Path::new("workspace"));
        let id = store.save("首行\n重要约束\n尾行").unwrap();
        let reopened = ToolOutputStore::new(&root, Path::new("workspace"));
        assert!(reopened.page(&id, 3, 4).unwrap().ends_with("重要约束"));
        assert!(reopened.page("../secret", 0, 100).is_err());
        let other = ToolOutputStore::new(&root, Path::new("other-workspace"));
        assert!(other.page(&id, 0, 100).is_err());
    }
}
