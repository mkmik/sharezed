//! Clean-room capture (PRD §7.1): a fresh `zsh -f`, S₀, source the bootstrap, S₁.

use crate::payload::zq;
use crate::state::{self, State};
use crate::store::R;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

/// ponytail: `zsh -f -i -c` rather than a real pty (§7.1 option B+). It sets
/// `interactive`, so `[[ -o interactive ]]` guards run; ZLE-dependent config
/// (`bindkey`, `zle -N`) still fails and prints to stderr. Upgrade to `zsh/zpty`
/// or a host pty if a real zshrc trips on it — that is PRD open question 1.
pub fn clean_room(bootstrap: &Path) -> R<(State, State)> {
    let dir = std::env::temp_dir().join(format!("sharezed-{}", std::process::id()));
    fs::create_dir_all(&dir)?;
    let (harness, o0, o1) = (dir.join("capture.zsh"), dir.join("s0"), dir.join("s1"));
    fs::write(&harness, include_str!("capture.zsh"))?;

    let status = Command::new("zsh")
        .args(["-f", "-i", "-c"])
        .arg(format!("source {}", zq(&harness.to_string_lossy())))
        .env("SZ_OUT0", &o0)
        .env("SZ_OUT1", &o1)
        .env("SZ_BOOT", bootstrap)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()?;

    let read = |p: &Path| -> R<State> {
        let buf = fs::read(p)
            .map_err(|e| format!("capture produced no state ({e}); zsh exited with {status}"))?;
        Ok(state::parse_wire(&buf)?)
    };
    let out = (read(&o0)?, read(&o1)?);
    let _ = fs::remove_dir_all(&dir);
    Ok(out)
}
