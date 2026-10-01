//! "Check for updates" / "Update now": asks GitHub for the latest release and
//! re-runs the regular installer, which replaces clipr and restarts it.
//! Uses the system's `curl` (macOS, Linux and Windows 10+ all have it).

use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

const REPO: &str = "Mudales/clipr";

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Idle,
    Checking,
    UpToDate,
    /// A newer version, e.g. "0.6.0".
    Available(String),
    Updating,
    Failed(String),
}

/// Shared with the background thread doing the check.
#[derive(Clone)]
pub struct Updater(Arc<Mutex<Status>>);

impl Default for Updater {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(Status::Idle)))
    }
}

pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// "1.2.10" → [1, 2, 10], for comparing versions numerically.
fn parse(v: &str) -> Vec<u64> {
    v.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    parse(latest) > parse(current)
}

fn latest_tag() -> Result<String, String> {
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "10", "-H", "Accept: application/vnd.github+json"])
        .arg(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    if !out.status.success() {
        return Err("couldn't reach GitHub (offline?)".into());
    }
    let json = String::from_utf8_lossy(&out.stdout);
    // Cheap parse: `"tag_name": "v0.6.0"`.
    json.split("\"tag_name\"")
        .nth(1)
        .and_then(|rest| rest.split('"').nth(1))
        .map(|tag| tag.trim_start_matches('v').to_owned())
        .ok_or_else(|| "unexpected reply from GitHub".into())
}

impl Updater {
    pub fn status(&self) -> Status {
        self.0.lock().unwrap().clone()
    }

    fn set(&self, s: Status) {
        *self.0.lock().unwrap() = s;
    }

    /// Checks in the background; `on_done` is called afterwards (to repaint).
    pub fn check(&self, on_done: impl FnOnce() + Send + 'static) {
        self.set(Status::Checking);
        let me = self.clone();
        std::thread::spawn(move || {
            me.set(match latest_tag() {
                Ok(tag) if is_newer(&tag, current()) => Status::Available(tag),
                Ok(_) => Status::UpToDate,
                Err(e) => Status::Failed(e),
            });
            on_done();
        });
    }

    /// Starts the installer detached. It stops this clipr, installs the new
    /// version and starts it, so this process is about to be replaced.
    pub fn install(&self) {
        let result = spawn_installer();
        self.set(match result {
            Ok(()) => Status::Updating,
            Err(e) => Status::Failed(format!("couldn't start the installer: {e}")),
        });
    }
}

#[cfg(unix)]
fn spawn_installer() -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let script = format!("curl -fsSL https://raw.githubusercontent.com/{REPO}/master/install.sh | sh");
    let log = crate::db::data_dir().join("update.log");
    let log = std::fs::File::create(log)?;
    Command::new("sh")
        .args(["-c", &script])
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0) // own group: survives the installer stopping clipr
        .spawn()?;
    Ok(())
}

#[cfg(windows)]
fn spawn_installer() -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    let script = format!("irm https://raw.githubusercontent.com/{REPO}/master/install.ps1 | iex");
    Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &script])
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("0.6.0", "0.5.1"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(!is_newer("0.5.1", "0.5.1"));
        assert!(!is_newer("0.5.0", "0.5.1"));
    }
}
