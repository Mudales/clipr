//! Actions (from Parcellite): run a command on a clip, then paste or copy the
//! result. Built-ins work everywhere; custom ones are shell commands that get
//! the clip on stdin and print the result on stdout.

use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(10);

/// What happens with the action's output.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Then {
    Paste,
    /// Type the result out as keystrokes (for apps that block paste).
    Type,
    Copy,
    /// Just run it (e.g. open a link); the output is ignored.
    Run,
}

impl Then {
    pub fn name(self) -> &'static str {
        match self {
            Then::Paste => "paste",
            Then::Type => "type",
            Then::Copy => "copy",
            Then::Run => "run",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "paste" => Some(Then::Paste),
            "type" => Some(Then::Type),
            "copy" => Some(Then::Copy),
            "run" => Some(Then::Run),
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct ClipAction {
    pub name: String,
    pub then: Then,
    /// A shell command, or `builtin:<name>` (see `BUILTINS`).
    pub command: String,
}

/// Built-in commands, usable as `builtin:<name>`.
pub const BUILTINS: &[(&str, &str)] = &[
    ("upper", "UPPERCASE"),
    ("lower", "lowercase"),
    ("trim", "Trim spaces at both ends"),
    ("join-lines", "Join lines into one"),
    ("open", "Open link / file"),
];

pub fn defaults() -> Vec<ClipAction> {
    let a = |name: &str, then, command: &str| ClipAction { name: name.into(), then, command: command.into() };
    vec![
        a("Open link", Then::Run, "builtin:open"),
        a("UPPERCASE", Then::Paste, "builtin:upper"),
        a("lowercase", Then::Paste, "builtin:lower"),
        a("Trim spaces", Then::Paste, "builtin:trim"),
        a("Join lines", Then::Paste, "builtin:join-lines"),
    ]
}

impl ClipAction {
    /// `Name | paste | command` (one line in settings.conf).
    pub fn to_line(&self) -> String {
        format!("{} | {} | {}", self.name.replace('|', "/"), self.then.name(), self.command)
    }

    pub fn from_line(line: &str) -> Option<Self> {
        let mut parts = line.splitn(3, '|');
        let name = parts.next()?.trim().to_owned();
        let then = Then::parse(parts.next()?)?;
        let command = parts.next()?.trim().to_owned();
        (!name.is_empty() && !command.is_empty()).then_some(Self { name, then, command })
    }

    /// Runs the action on `text`; returns the output (`None` for `Then::Run`).
    pub fn run(&self, text: &str) -> Result<Option<String>> {
        let out = match self.command.strip_prefix("builtin:") {
            Some(b) => builtin(b.trim(), text)?,
            None => shell(&self.command, text)?,
        };
        Ok((self.then != Then::Run).then_some(out))
    }
}

fn builtin(name: &str, text: &str) -> Result<String> {
    Ok(match name {
        "upper" => text.to_uppercase(),
        "lower" => text.to_lowercase(),
        "trim" => text.trim().to_owned(),
        "join-lines" => text.split_whitespace().collect::<Vec<_>>().join(" "),
        "open" => {
            let target = text.trim();
            let opener = if cfg!(target_os = "macos") {
                Command::new("open").arg(target).spawn()
            } else if cfg!(windows) {
                Command::new("explorer").arg(target).spawn()
            } else {
                Command::new("xdg-open").arg(target).spawn()
            };
            opener.with_context(|| format!("couldn't open {target}"))?;
            String::new()
        }
        other => bail!("unknown built-in action {other:?}"),
    })
}

fn shell(command: &str, input: &str) -> Result<String> {
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    };
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("couldn't run {command:?}"))?;
    // Feed stdin from a thread, so a command that doesn't read it can't block us.
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_owned();
    std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("{command:?} took longer than {}s", TIMEOUT.as_secs());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let out = reader.join().unwrap_or_default();
    if !status.success() {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        bail!("{command:?} failed: {}", err.trim());
    }
    let mut out = String::from_utf8_lossy(&out).into_owned();
    // Commands usually end with a newline; don't paste it.
    if out.ends_with('\n') {
        out.pop();
        if out.ends_with('\r') {
            out.pop();
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_round_trip() {
        for a in defaults() {
            assert_eq!(ClipAction::from_line(&a.to_line()), Some(a));
        }
        assert!(ClipAction::from_line("broken").is_none());
    }

    #[test]
    fn builtins_and_shell() {
        let up = ClipAction { name: "u".into(), then: Then::Paste, command: "builtin:upper".into() };
        assert_eq!(up.run("shalom").unwrap().as_deref(), Some("SHALOM"));
        let join = ClipAction { name: "j".into(), then: Then::Copy, command: "builtin:join-lines".into() };
        assert_eq!(join.run(" a\n b \n").unwrap().as_deref(), Some("a b"));
        #[cfg(unix)]
        {
            let rev = ClipAction { name: "r".into(), then: Then::Paste, command: "tr a-z A-Z".into() };
            assert_eq!(rev.run("abc").unwrap().as_deref(), Some("ABC"));
            let bad = ClipAction { name: "b".into(), then: Then::Paste, command: "exit 3".into() };
            assert!(bad.run("x").is_err());
        }
    }
}
