//! Atomic replacement for complete local projections.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub enum ReadProjectionError {
    NotPlainFile,
    Io(io::Error),
}

pub fn read_projection(path: &Path) -> Result<Option<Vec<u8>>, ReadProjectionError> {
    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ReadProjectionError::Io(error)),
    };
    if metadata.is_symlink() {
        match path.metadata() {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(ReadProjectionError::Io(error)),
            Ok(_) => return Err(ReadProjectionError::NotPlainFile),
        }
    }
    if !metadata.is_file() {
        return Err(ReadProjectionError::NotPlainFile);
    }
    fs::read(path).map(Some).map_err(ReadProjectionError::Io)
}

pub fn projection_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

pub fn replace_projection(path: &Path, data: &[u8]) -> io::Result<()> {
    publish(path, data, |temporary, path| fs::rename(temporary, path))
}

/// Publish a fully written file under an exclusive name. Concurrent readers
/// never observe a partial record. Existing records are never overwritten.
pub fn create_projection(path: &Path, data: &[u8]) -> io::Result<()> {
    publish(path, data, |temporary, path| fs::hard_link(temporary, path))
}

fn publish(
    path: &Path,
    data: &[u8],
    install: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let parent = projection_parent(path);
    let mut create = OpenOptions::new();
    create.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        create.mode(0o600);
    }
    let (temporary, mut file) = loop {
        let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let name = format!(".syzygy-{}-{sequence}.tmp", std::process::id());
        let temporary = parent.join(name);
        match create.open(&temporary) {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };

    let written = file.write_all(data).and_then(|()| file.sync_all());
    drop(file);
    let installed = written.and_then(|()| install(&temporary, path));
    let _ = fs::remove_file(&temporary);
    installed?;
    sync_parent(parent)
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> io::Result<()> {
    fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> io::Result<()> {
    Ok(())
}
