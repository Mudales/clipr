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
    /// The installer is running (since then).
    Updating(std::time::Instant),
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

fn result_ok(s: &Status) -> bool {
    matches!(s, Status::Updating(_))
}

fn updated_marker() -> std::path::PathBuf {
    crate::db::data_dir().join("updated_from")
}

/// After a self-update: "Updated to 0.8.4" (once), for the picker to show.
pub fn take_updated_notice() -> Option<String> {
    let from = std::fs::read_to_string(updated_marker()).ok()?;
    let _ = std::fs::remove_file(updated_marker());
    (from.trim() != current()).then(|| format!("Updated to clipr {}", current()))
}

/// Whether we were just updated (without consuming the notice).
pub fn just_updated() -> bool {
    updated_marker().exists()
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

    /// Starts the installer detached and quits this clipr, so nothing has to
    /// stop it (the installer installs the new version and starts it).
    pub fn install(&self) {
        let result = spawn_installer();
        self.set(match result {
            Ok(()) => Status::Updating(std::time::Instant::now()),
            Err(e) => Status::Failed(format!("couldn't start the installer: {e}")),
        });
        if result_ok(&self.status()) {
            let _ = std::fs::write(updated_marker(), current());
            // Get out of the installer's way.
            quit_soon();
        }
    }
}

/// Stops every clipr (daemon and picker) and starts it again, from a detached
/// helper that outlives us; this process quits by itself right after (so the
/// helper doesn't have to find it). Also finishes an update whose restart got lost.
pub fn restart() -> std::io::Result<()> {
    spawn_restart_helper()?;
    quit_soon();
    Ok(())
}

/// Quits this clipr: the window closes normally from the UI thread (calling
/// exit() from another thread can leave a Wayland window hung half-way,
/// which Hyprland reports as "not responding"). If that hasn't happened
/// within a few seconds, leave immediately without running any cleanup.
pub(crate) fn quit_soon() {
    crate::ui::request_quit();
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(4));
        #[cfg(unix)]
        unsafe {
            libc::_exit(0);
        }
        #[cfg(windows)]
        std::process::exit(0);
    });
}

fn spawn_restart_helper() -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let start = {
        // …/clipr.app/Contents/MacOS/clipr → open -a …/clipr.app (a plain
        // binary, e.g. from a source build, is started directly).
        let exe = std::env::current_exe()?;
        match exe.ancestors().nth(3).filter(|a| a.extension().is_some_and(|e| e == "app")) {
            Some(app) => format!("open -a '{}'", app.display()),
            None => format!("nohup '{}' >/dev/null 2>&1 &", exe.display()),
        }
    };
    #[cfg(target_os = "linux")]
    let start = format!("nohup '{}' >/dev/null 2>&1 &", std::env::current_exe()?.display());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Command::new("sh")
            // -a: macOS's pkill otherwise skips its parent (this clipr).
            .args(["-c", &format!("sleep 0.6; pkill {} -x clipr; sleep 1; {start}", if cfg!(target_os = "macos") { "-a" } else { "" })])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let exe = std::env::current_exe()?;
        let script = format!(
            "Start-Sleep -Milliseconds 300; Stop-Process -Name clipr -Force; Start-Sleep 1; Start-Process '{}'",
            exe.display()
        );
        Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .creation_flags(0x0800_0000 | 0x0000_0008) // no window, detached
            .spawn()?;
    }
    Ok(())
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
