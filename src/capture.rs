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
    /// Every file the bootstrap sourced, in load order, deduplicated.
    pub sources: Vec<PathBuf>,
}

/// ponytail: `zsh -f -i -c` rather than a real pty (§7.1 option B+). It sets
/// `interactive`, so `[[ -o interactive ]]` guards run; ZLE-dependent config
/// (`bindkey`, `zle -N`) still fails and prints to stderr. Upgrade to `zsh/zpty`
/// or a host pty if a real zshrc trips on it — that is PRD open question 1.
pub fn clean_room(bootstrap: &Path) -> R<Capture> {
    let dir = std::env::temp_dir().join(format!("sharezed-{}", std::process::id()));
    fs::create_dir_all(&dir)?;
    let (harness, o0, o1) = (dir.join("capture.zsh"), dir.join("s0"), dir.join("s1"));
    fs::write(&harness, include_str!("capture.zsh"))?;

    let out = Command::new("zsh")
        .args(["-f", "-i", "-c"])
        .arg(format!("source {}", zq(&harness.to_string_lossy())))
        .env("SZ_OUT0", &o0)
        .env("SZ_OUT1", &o1)
        .env("SZ_BOOT", bootstrap)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;

    // The bootstrap's own diagnostics still belong on our stderr.
    let (sources, stderr) = split_source_trace(&String::from_utf8_lossy(&out.stderr));
    eprint!("{stderr}");

    let read = |p: &Path| -> R<State> {
        let buf = fs::read(p).map_err(|e| {
            format!(
                "capture produced no state ({e}); zsh exited with {}",
                out.status
            )
        })?;
        Ok(state::parse_wire(&buf)?)
    };
    let cap = Capture {
        s0: read(&o0)?,
        s1: read(&o1)?,
        sources,
    };
    let _ = fs::remove_dir_all(&dir);
    Ok(cap)
}

/// SOURCE_TRACE emits `+<path>:<line>> <sourcetrace>` per file loaded. Returns
/// the paths and whatever else the bootstrap wrote to stderr.
fn split_source_trace(stderr: &str) -> (Vec<PathBuf>, String) {
    let (mut sources, mut rest) = (Vec::new(), String::new());
    for line in stderr.lines() {
        let path = line
            .strip_suffix("> <sourcetrace>")
            .and_then(|l| l.trim_start_matches(['+', '#']).rsplit_once(':'))
            .map(|(path, _lineno)| PathBuf::from(path));
        match path {
            Some(p) if !sources.contains(&p) => sources.push(p),
            Some(_) => {}
            None => {
                rest.push_str(line);
                rest.push('\n');
            }
        }
    }
    (sources, rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_trace_is_split_from_real_diagnostics() {
        let (src, rest) = split_source_trace(
            "+/home/m/.zshrc:1> <sourcetrace>\n\
             zsh: command not found: nope\n\
             +/home/m/.zsh/zmac:1> <sourcetrace>\n\
             +/dev/fd/12:1> <sourcetrace>\n\
             +/home/m/.zshrc:1> <sourcetrace>\n",
        );
        assert_eq!(
            src,
            [
                PathBuf::from("/home/m/.zshrc"),
                PathBuf::from("/home/m/.zsh/zmac"),
                PathBuf::from("/dev/fd/12"),
            ],
            "in load order, deduplicated"
        );
        assert_eq!(rest, "zsh: command not found: nope\n");
    }
}
