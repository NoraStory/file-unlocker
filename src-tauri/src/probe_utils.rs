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
pub(crate) fn create_probe(dir: &Path, prefix: &str) -> io::Result<(PathBuf, File)> {
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
            // 句柄关闭/进程异常退出时由内核自动删除，避免自检崩溃后残留探针
            .custom_flags(0x0400_0000) // FILE_FLAG_DELETE_ON_CLOSE
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
    fn probe_paths_are_unique_and_cleaned() {
        let dir = std::env::temp_dir();
        let probe1 = ProbeFile::create(&dir, "fu_probe_test").unwrap();
        let probe2 = ProbeFile::create(&dir, "fu_probe_test").unwrap();

        assert_ne!(probe1.path(), probe2.path());
        assert!(probe1.path().exists());
        assert!(probe2.path().exists());

        let path1 = probe1.path().to_path_buf();
        let path2 = probe2.path().to_path_buf();
        drop(probe1);
        drop(probe2);

        assert!(!path1.exists());
        assert!(!path2.exists());
    }
}

/// 探针文件 RAII：Drop 时先释放句柄再删除文件，
/// 自检中途失败/早退也不会在临时目录残留垃圾。
pub(crate) struct ProbeFile {
    path: PathBuf,
    file: Option<File>,
}

impl ProbeFile {
    pub(crate) fn create(dir: &Path, prefix: &str) -> io::Result<Self> {
        let (path, file) = create_probe(dir, prefix)?;
        Ok(Self {
            path,
            file: Some(file),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ProbeFile {
    fn drop(&mut self) {
        // 必须先 drop 内部句柄再删除，否则独占打开时删除失败
        drop(self.file.take());
        let _ = std::fs::remove_file(&self.path);
    }
}

impl std::ops::Deref for ProbeFile {
    type Target = File;

    fn deref(&self) -> &Self::Target {
        self.file.as_ref().expect("probe file is alive")
    }
}

impl std::ops::DerefMut for ProbeFile {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.file.as_mut().expect("probe file is alive")
    }
}
