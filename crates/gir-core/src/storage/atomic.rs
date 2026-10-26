use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::NamedTempFile;

use crate::error::GirError;

/// Default lock contention timeout (100ms, matching Git's `core.filesRefLockTimeout`).
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_millis(100);

/// Flushes a directory's metadata/entries to physical storage via `fsync`.
pub fn fsync_dir(dir: &Path) -> io::Result<()> {
    let dir_file = File::open(dir)?;
    dir_file.sync_all()
}

/// An exclusive POSIX lockfile (`<target>.lock`) opened with `O_CREAT | O_EXCL`.
///
/// Guarantees atomic file updates:
/// 1. Writes go to `<target>.lock`.
/// 2. `commit()` flushes buffers, calls `fsync` on the file, renames `<target>.lock`
///    over `<target>`, and calls `fsync` on the parent directory.
/// 3. If dropped before `commit()`, the `.lock` file is automatically removed.
pub struct LockFile {
    target_path: PathBuf,
    lock_path: PathBuf,
    writer: Option<BufWriter<File>>,
    committed: bool,
}

impl LockFile {
    /// Acquires `<target_path>.lock` using the default 100ms quadratic retry backoff.
    pub fn acquire(target_path: impl AsRef<Path>) -> Result<Self, GirError> {
        Self::acquire_with_timeout(target_path, DEFAULT_LOCK_TIMEOUT)
    }

    /// Acquires `<target_path>.lock` waiting up to `timeout` with quadratic backoff
    /// (`1ms, 4ms, 9ms, 16ms, 25ms...`) if another `git` or `gir` process holds the lock.
    pub fn acquire_with_timeout(
        target_path: impl AsRef<Path>,
        timeout: Duration,
    ) -> Result<Self, GirError> {
        let target_path = target_path.as_ref().to_path_buf();
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut lock_os = target_path.as_os_str().to_os_string();
        lock_os.push(".lock");
        let lock_path = PathBuf::from(lock_os);

        let start = Instant::now();
        let mut attempt: u64 = 1;

        let file = loop {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(f) => break f,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let elapsed = start.elapsed();
                    if elapsed >= timeout {
                        return Err(GirError::LockContention { path: target_path });
                    }
                    let sleep_ms = (attempt * attempt).min(50);
                    let remaining = timeout.saturating_sub(elapsed);
                    thread::sleep(Duration::from_millis(sleep_ms).min(remaining));
                    attempt += 1;
                }
                Err(e) => return Err(GirError::Io(e)),
            }
        };

        Ok(Self {
            target_path,
            lock_path,
            writer: Some(BufWriter::new(file)),
            committed: false,
        })
    }

    /// Returns the path of the underlying `.lock` file.
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    /// Flushes, `fsync`s, atomically renames `<target>.lock` to `<target>`, and `fsync`s the parent directory.
    pub fn commit(mut self) -> Result<(), GirError> {
        if let Some(mut buf_writer) = self.writer.take() {
            buf_writer.flush()?;
            let file = buf_writer
                .into_inner()
                .map_err(|e| GirError::Io(e.into_error()))?;
            file.sync_all()?;
        }

        fs::rename(&self.lock_path, &self.target_path)?;
        if let Some(parent) = self.target_path.parent() {
            let _ = fsync_dir(parent);
        }
        self.committed = true;
        Ok(())
    }
}

impl Write for LockFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer
            .as_mut()
            .expect("LockFile writer active")
            .write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer
            .as_mut()
            .expect("LockFile writer active")
            .flush()
    }
}

impl Drop for LockFile {
    fn drop(&mut self) {
        // Close the file handle first before unlinking the lockfile.
        drop(self.writer.take());
        if !self.committed {
            let _ = fs::remove_file(&self.lock_path);
        }
    }
}

/// Atomically writes an immutable loose object file (`0444` permissions) via a temporary file
/// in the same directory followed by `fsync`, `rename`, and parent directory `fsync`.
pub fn write_immutable_atomic(target_path: &Path, bytes: &[u8]) -> Result<(), GirError> {
    if target_path.exists() {
        return Ok(());
    }

    let parent = target_path
        .parent()
        .ok_or_else(|| GirError::InvalidArgument("target path has no parent dir".to_string()))?;
    fs::create_dir_all(parent)?;

    let mut temp = NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.flush()?;
    temp.as_file().sync_all()?;

    // Loose Git objects are immutable; set 0444 (read-only) permissions.
    let mut perms = temp.as_file().metadata()?.permissions();
    perms.set_mode(0o444);
    temp.as_file().set_permissions(perms)?;

    match temp.persist(target_path) {
        Ok(_) => {
            let _ = fsync_dir(parent);
            Ok(())
        }
        Err(persist_err) if persist_err.error.kind() == io::ErrorKind::AlreadyExists => {
            // Content-addressed object already written concurrently; safe to succeed.
            Ok(())
        }
        Err(persist_err) => Err(GirError::Io(persist_err.error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn lockfile_cleans_up_on_drop_without_commit() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("HEAD");
        fs::write(&target, "original\n").unwrap();

        {
            let mut lock = LockFile::acquire(&target).unwrap();
            lock.write_all(b"aborted\n").unwrap();
            assert!(dir.path().join("HEAD.lock").exists());
            // Dropped here without calling .commit()
        }

        assert!(!dir.path().join("HEAD.lock").exists());
        assert_eq!(fs::read_to_string(&target).unwrap(), "original\n");
    }

    #[test]
    fn lockfile_commits_atomically() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("HEAD");

        let mut lock = LockFile::acquire(&target).unwrap();
        lock.write_all(b"ref: refs/heads/main\n").unwrap();
        lock.commit().unwrap();

        assert!(!dir.path().join("HEAD.lock").exists());
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "ref: refs/heads/main\n"
        );
    }
}
