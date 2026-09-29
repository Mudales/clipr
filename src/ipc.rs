//! Tiny line-based protocol between `clipr pick`/`clipr toggle` and the daemon.
//!
//! Commands: `TOGGLE`, `PASTE <ids>`, `TYPE <ids>`, `COPY <ids>`.
//!
//! Unix: a socket only this user can open. Windows (no Unix sockets in std):
//! a loopback TCP port plus a random token, both written to a file in the
//! user's data folder; commands without the token are ignored.

use anyhow::Result;
use std::io::{BufRead, BufReader, Write};

#[cfg(unix)]
mod imp {
    use anyhow::{Context, Result};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    pub type Stream = UnixStream;
    pub struct Listener(UnixListener);

    pub fn socket_path() -> PathBuf {
        #[cfg(target_os = "linux")]
        if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
            return PathBuf::from(dir).join("clipr.sock");
        }
        crate::db::data_dir().join("clipr.sock")
    }

    pub fn listen() -> Result<Listener> {
        let path = socket_path();
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        eprintln!("clipr: listening on {}", path.display());
        Ok(Listener(listener))
    }

    impl Listener {
        pub fn accept(&self) -> Option<Stream> {
            self.0.accept().ok().map(|(s, _)| s)
        }
        pub fn check(&self, line: &str) -> Option<String> {
            Some(line.to_owned())
        }
    }

    pub fn connect() -> Option<(Stream, String)> {
        UnixStream::connect(socket_path()).ok().map(|s| (s, String::new()))
    }
}

#[cfg(windows)]
mod imp {
    use anyhow::Result;
    use std::hash::{BuildHasher, Hasher};
    use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::time::Duration;

    pub type Stream = TcpStream;
    pub struct Listener {
        listener: TcpListener,
        token: String,
    }

    fn port_file() -> PathBuf {
        crate::db::data_dir().join("clipr.port")
    }

    pub fn listen() -> Result<Listener> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u32(std::process::id());
        let token = format!("{:016x}", h.finish());
        std::fs::write(port_file(), format!("{} {token}", listener.local_addr()?.port()))?;
        eprintln!("clipr: listening on {}", listener.local_addr()?);
        Ok(Listener { listener, token })
    }

    impl Listener {
        pub fn accept(&self) -> Option<Stream> {
            let (stream, addr) = self.listener.accept().ok()?;
            addr.ip().is_loopback().then_some(stream)
        }
        /// Strips and verifies the token; `None` = reject.
        pub fn check(&self, line: &str) -> Option<String> {
            let (token, rest) = line.split_once(' ').unwrap_or((line, ""));
            (token == self.token).then(|| rest.to_owned())
        }
    }

    pub fn connect() -> Option<(Stream, String)> {
        let text = std::fs::read_to_string(port_file()).ok()?;
        let (port, token) = text.trim().split_once(' ')?;
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port.parse::<u16>().ok()?));
        let stream = TcpStream::connect_timeout(&addr, Duration::from_millis(300)).ok()?;
        Some((stream, format!("{token} ")))
    }
}

pub use imp::Listener;

pub fn listen() -> Result<Listener> {
    imp::listen()
}

impl Listener {
    /// Blocks forever, calling `handle` with each command received.
    pub fn serve(&self, mut handle: impl FnMut(&str)) {
        loop {
            let Some(stream) = self.accept() else { continue };
            let mut line = String::new();
            if BufReader::new(stream).read_line(&mut line).is_err() {
                continue;
            }
            if let Some(cmd) = self.check(line.trim_end()) {
                handle(cmd.trim());
            }
        }
    }
}

pub fn send(cmd: &str) -> Result<()> {
    let (mut stream, prefix) = imp::connect().ok_or_else(|| anyhow::anyhow!("clipr daemon is not running"))?;
    stream.write_all(format!("{prefix}{cmd}\n").as_bytes())?;
    Ok(())
}

pub fn daemon_running() -> bool {
    imp::connect().is_some()
}
