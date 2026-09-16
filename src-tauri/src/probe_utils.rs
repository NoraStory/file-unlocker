use std::{
    fs::{File, OpenOptions},
    io,
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static PROBE_SEQ: AtomicU64 = AtomicU64::new(0);

/// 创建一个全新且独占打开的探针文件。
///
/// 旧版使用 `进程 ID + 固定文件名`，进程 ID 复用时可能撞上残留探针
/// （或安全软件的短时扫描句柄），导致 os error 32。这里每次都用
/// `create_new` 生成新路径，避免与历史文件或并发自检冲突。
pub fn create_probe(dir: &Path, prefix: &str) -> io::Result<(PathBuf, File)> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?
        .as_nanos() as u64;

    for attempt in 0..8u64 {
        let seq = PROBE_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(
            "{prefix}_{}_{:016x}_{:016x}_{:02x}.tmp",
            std::process::id(),
            elapsed,
            seq,
            attempt
        ));

        match OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "探针文件路径生成失败（连续冲突）",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_paths_are_unique() {
        let dir = std::env::temp_dir();
        let (path1, file1) = create_probe(&dir, "fu_probe_test").unwrap();
        let (path2, file2) = create_probe(&dir, "fu_probe_test").unwrap();

        assert_ne!(path1, path2);
        assert!(path1.exists());
        assert!(path2.exists());

        drop(file1);
        drop(file2);
        let _ = std::fs::remove_file(&path1);
        let _ = std::fs::remove_file(&path2);
    }
}
