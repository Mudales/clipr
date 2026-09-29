//! Tiny line-based protocol between `clipr pick`/`clipr toggle` and the daemon.
//!
//! Commands: `TOGGLE`, `PASTE <id>`, `TYPE <id>`, `COPY <id>`.

use anyhow::Result;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

pub fn socket_path() -> PathBuf {
    #[cfg(target_os = "linux")]
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("clipr.sock");
    }
    crate::db::data_dir().join("clipr.sock")
}

pub fn send(cmd: &str) -> Result<()> {
    let mut stream = UnixStream::connect(socket_path())?;
    stream.write_all(cmd.as_bytes())?;
    stream.write_all(b"\n")?;
    Ok(())
}

pub fn daemon_running() -> bool {
    UnixStream::connect(socket_path()).is_ok()
}
