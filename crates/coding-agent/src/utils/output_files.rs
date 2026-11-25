//! 对应上游 `src/utils/output-files.ts`：工具要把「没给模型看全」的输出留下来时写到这里。
//!
//! 上游写到系统临时目录，文件名是 `<prefix>-<random hex><扩展名>`，权限 0600。这里一样。

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::PathBuf;

/// pi: `OUTPUT_FILE_MODE`
#[cfg(unix)]
const OUTPUT_FILE_MODE: u32 = 0o600;

fn random_bytes() -> [u8; 8] {
    let mut bytes = [0u8; 8];
    if let Ok(mut file) = File::open("/dev/urandom") {
        if file.read_exact(&mut bytes).is_ok() {
            return bytes;
        }
    }
    // 拿不到系统随机数时用时间与计数器兜底：只要求路径不撞，不用于安全。
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    let value = nanos.wrapping_add(COUNTER.fetch_add(1, Ordering::SeqCst));
    bytes.copy_from_slice(&value.to_le_bytes());
    bytes
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// pi: `createOutputFilePath`
pub fn create_output_file_path(prefix: &str, extension: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{prefix}-{}{extension}", hex(&random_bytes())))
}

fn open_new(path: &PathBuf) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(OUTPUT_FILE_MODE);
    }
    options.open(path)
}

/// pi: `writeOutputFile`
pub async fn write_output_file(prefix: &str, extension: &str, data: &[u8]) -> std::io::Result<PathBuf> {
    let path = create_output_file_path(prefix, extension);
    let path_for_write = path.clone();
    let data = data.to_vec();
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let mut file = open_new(&path_for_write)?;
        file.write_all(&data)
    })
    .await
    .map_err(|error| std::io::Error::other(error.to_string()))??;
    Ok(path)
}

/// pi: `createOutputFileStream`
pub fn create_output_file_stream(
    prefix: &str,
    extension: &str,
) -> std::io::Result<(PathBuf, File)> {
    let path = create_output_file_path(prefix, extension);
    let file = open_new(&path)?;
    Ok((path, file))
}
