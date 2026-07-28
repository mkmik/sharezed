//! The append-only log (PRD §7.3) and its trust checks (§7.7).

use crate::state::{self, Change, State};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// ponytail: entries are never GC'd, only old snapshots are. That keeps
/// `diff N`/`revert N` working forever and removes the "cursor predates the
/// oldest entry" catch-up path. Revisit if a busy channel gets fat.
const SNAPSHOT_EVERY: u64 = 20;

#[derive(Serialize, Deserialize, Default)]
pub struct Meta {
    #[serde(default)]
    pub bootstrap: String,
    /// Every file the bootstrap sourced, path -> sha256. The trust gate (§7.7)
    /// compares the whole set: editing a sourced file has to trip it too.
    #[serde(default)]
    pub sources: std::collections::BTreeMap<String, String>,
}

pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    pub fn open(channel: &str) -> R<Store> {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/state"));
        let dir = base.join("sharezed").join(channel);
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        // The log is a code-execution channel into every shell you own.
        let md = fs::metadata(&dir)?;
        if md.uid() != unsafe { libc::getuid() } {
            return Err(format!("{} is owned by uid {}, not you", dir.display(), md.uid()).into());
        }
        if md.mode() & 0o077 != 0 {
            return Err(format!("{} is group/world accessible", dir.display()).into());
        }
        Ok(Store { dir })
    }

    pub fn head_path(&self) -> PathBuf {
        self.dir.join("head")
    }

    pub fn head(&self) -> u64 {
        fs::read_to_string(self.head_path())
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    fn entry_path(&self, seq: u64) -> PathBuf {
        self.dir.join(format!("{seq:06}.jsonl"))
    }

    fn snapshot_path(&self, seq: u64) -> PathBuf {
        self.dir.join(format!("snapshot-{seq:06}.jsonl"))
    }

    pub fn entry(&self, seq: u64) -> R<Vec<Change>> {
        read_jsonl(&self.entry_path(seq))
    }

    fn snapshots(&self) -> Vec<u64> {
        let mut v: Vec<u64> = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                e.file_name()
                    .to_str()?
                    .strip_prefix("snapshot-")?
                    .strip_suffix(".jsonl")?
                    .parse()
                    .ok()
            })
            .collect();
        v.sort_unstable();
        v
    }

    /// The published desired state at `seq`: newest snapshot at or before it,
    /// then replay.
    pub fn desired(&self, seq: u64) -> R<State> {
        let mut st = State::new();
        let snap = self.snapshots().into_iter().rfind(|s| *s <= seq);
        let start = match snap {
            Some(s) => {
                state::apply(&mut st, &read_jsonl(&self.snapshot_path(s))?);
                s
            }
            None => 0,
        };
        for n in start + 1..=seq {
            state::apply(&mut st, &self.entry(n)?);
        }
        Ok(st)
    }

    /// Write the entry durably, then bump `head` — so a shell never sees a seq
    /// whose entry isn't on disk yet.
    pub fn publish(&self, changes: &[Change], desired: &State) -> R<u64> {
        let seq = self.head() + 1;
        write_atomic(&self.entry_path(seq), &to_jsonl(changes)?)?;
        if seq.is_multiple_of(SNAPSHOT_EVERY) {
            let full = state::diff(&State::new(), desired);
            write_atomic(&self.snapshot_path(seq), &to_jsonl(&full)?)?;
            for old in self.snapshots().into_iter().filter(|s| *s < seq) {
                let _ = fs::remove_file(self.snapshot_path(old));
            }
        }
        write_atomic(&self.head_path(), format!("{seq}\n").as_bytes())?;
        Ok(seq)
    }

    /// Held for the duration of a publish; two `reload`s can race (§11.5).
    pub fn lock(&self) -> R<File> {
        let f = File::create(self.dir.join(".lock"))?;
        if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&f), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(f)
    }

    pub fn meta(&self) -> Meta {
        fs::read_to_string(self.dir.join("meta.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_meta(&self, m: &Meta) -> R {
        write_atomic(
            &self.dir.join("meta.json"),
            serde_json::to_string_pretty(m)?.as_bytes(),
        )
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn to_jsonl(changes: &[Change]) -> R<Vec<u8>> {
    let mut out = Vec::new();
    for c in changes {
        serde_json::to_writer(&mut out, c)?;
        out.push(b'\n');
    }
    Ok(out)
}

fn read_jsonl(path: &Path) -> R<Vec<Change>> {
    let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    BufReader::new(f)
        .lines()
        .map(|l| Ok(serde_json::from_str(&l?)?))
        .collect()
}

fn write_atomic(path: &Path, data: &[u8]) -> R {
    let tmp = path.with_extension("tmp");
    let mut f = File::create(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path)?;
    Ok(())
}
