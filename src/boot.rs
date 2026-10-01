//! Detects that the computer restarted since clipr last ran (for Settings →
//! "Clear history after restart"). Restarting clipr itself, e.g. for an
//! update, doesn't count.

/// When the system booted, in seconds since 1970.
fn boot_time() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string("/proc/stat").ok()?;
        return stat.lines().find_map(|l| l.strip_prefix("btime ")?.trim().parse().ok());
    }
    #[cfg(target_os = "macos")]
    {
        // "{ sec = 1790701818, usec = 123 } Wed Oct  1 …"
        let out = std::process::Command::new("sysctl").args(["-n", "kern.boottime"]).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        return text.split("sec = ").nth(1)?.split(',').next()?.trim().parse().ok();
    }
    #[cfg(windows)]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
        let up = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() } / 1000;
        return Some(now.saturating_sub(up));
    }
    #[allow(unreachable_code)]
    None
}

/// True the first time it's called after each reboot (records the boot time).
pub fn is_new_boot() -> bool {
    let Some(now) = boot_time() else { return false };
    let path = crate::db::data_dir().join("last_boot");
    let last: Option<u64> = std::fs::read_to_string(&path).ok().and_then(|s| s.trim().parse().ok());
    let _ = std::fs::write(&path, now.to_string());
    // A first run (no record) isn't a restart. Allow some slack: on Windows
    // the boot time is computed from the uptime and wobbles a little.
    last.is_some_and(|last| now.abs_diff(last) > 30)
}
