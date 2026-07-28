//! Clean-room capture (PRD §7.1): a fresh `zsh -f`, S₀, source the bootstrap, S₁.

use crate::payload::zq;
use crate::state::{self, State};
use crate::store::R;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Capture {
    pub s0: State,
    pub s1: State,
    /// Files the bootstrap sourced, in load order.
    pub sources: Vec<PathBuf>,
    /// External commands it ran, resolved against the bootstrap's own PATH.
    pub commands: Vec<PathBuf>,
}

/// ponytail: `zsh -f -i -c` rather than a real pty (§7.1 option B+). It sets
/// `interactive`, so `[[ -o interactive ]]` guards run; ZLE-dependent config
/// (`bindkey`, `zle -N`) still fails and prints to stderr. Upgrade to `zsh/zpty`
/// or a host pty if a real zshrc trips on it — that is PRD open question 1.
pub fn clean_room(bootstrap: &Path) -> R<Capture> {
    let dir = std::env::temp_dir().join(format!("sharezed-{}", std::process::id()));
    fs::create_dir_all(&dir)?;
    let f = |n: &str| dir.join(n);
    fs::write(f("capture.zsh"), include_str!("capture.zsh"))?;

    let status = Command::new("zsh")
        .args(["-f", "-i", "-c"])
        .arg(format!(
            "source {}",
            zq(&f("capture.zsh").to_string_lossy())
        ))
        .env("SZ_OUT0", f("s0"))
        .env("SZ_OUT1", f("s1"))
        .env("SZ_SRC", f("src"))
        .env("SZ_CMDS", f("cmds"))
        .env("SZ_TRACE", f("trace"))
        .env("SZ_BOOT", bootstrap)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()?;

    let state = |p: PathBuf| -> R<State> {
        let buf = fs::read(&p)
            .map_err(|e| format!("capture produced no state ({e}); zsh exited with {status}"))?;
        Ok(state::parse_wire(&buf)?)
    };
    // Deduplicated, order preserved: the first load is the one that mattered.
    let list = |p: PathBuf| -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for l in fs::read_to_string(&p).unwrap_or_default().lines() {
            let l = PathBuf::from(l);
            if !out.contains(&l) {
                out.push(l);
            }
        }
        out
    };
    let cap = Capture {
        s0: state(f("s0"))?,
        s1: state(f("s1"))?,
        sources: list(f("src")),
        commands: list(f("cmds")),
    };
    let _ = fs::remove_dir_all(&dir);
    Ok(cap)
}
