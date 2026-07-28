//! sharezed — direnv for everything a shell knows. See docs/PRD.md.

mod capture;
mod merge;
mod payload;
mod state;
mod store;

use clap::{Parser, Subcommand};
use state::{Item, Kind, State};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use store::{R, Store};

/// direnv for everything a shell knows.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Named stream of entries to publish to / read from.
    #[arg(long, global = true, default_value = "user")]
    channel: String,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Clean-room capture of the bootstrap script; publish the delta.
    Reload {
        /// Trust a changed bootstrap script without a separate `allow`.
        #[arg(long)]
        allow: bool,
    },
    /// Cursor vs head, pending entries, conflicts.
    Status,
    /// Human-readable view of an entry (default: the newest).
    Diff { seq: Option<u64> },
    /// List generations.
    Log,
    /// Emit the apply payload for the current shell — the eval'd thing.
    Export {
        #[arg(value_parser = ["zsh"])]
        shell: String,
        #[arg(long, default_value_t = 0)]
        cursor: u64,
    },
    /// Print the shell integration; eval it in .zshrc.
    Hook {
        #[arg(value_parser = ["zsh"])]
        shell: String,
    },
    /// Trust the current content of the bootstrap script.
    Allow,
    /// Publish the inverse of an entry.
    Revert { seq: u64 },
    /// Show how this shell's PATH was merged.
    Path {
        #[command(subcommand)]
        cmd: PathCmd,
    },
    /// Diagnose hook order, perms, stale cursors, dead PATH dirs.
    Doctor {
        /// Also report PATH entries whose directory is gone (§8.10).
        #[arg(long)]
        prune_missing: bool,
    },
}

#[derive(Subcommand)]
enum PathCmd {
    /// Entry-by-entry account of the local vs published PATH.
    Explain,
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(&cli) {
        eprintln!("sharezed: {e}");
        std::process::exit(1);
    }
}

fn run(cli: &Cli) -> R {
    let ch = &cli.channel;
    match &cli.command {
        Cmd::Reload { allow } => reload(ch, *allow),
        Cmd::Status => status(ch),
        Cmd::Diff { seq } => diff(ch, *seq),
        Cmd::Log => log(ch),
        Cmd::Export { cursor, .. } => export(ch, *cursor),
        Cmd::Hook { .. } => hook(ch),
        Cmd::Allow => allow(ch),
        Cmd::Revert { seq } => revert(ch, *seq),
        Cmd::Path { .. } => path_explain(ch),
        Cmd::Doctor { prune_missing } => doctor(ch, *prune_missing),
    }
}

// --- config -----------------------------------------------------------------
// ponytail: env vars, no config file. The PRD's TOML (§8.4, §8.7) buys a parser
// dependency for values nobody has needed to change yet.

fn bootstrap() -> PathBuf {
    std::env::var_os("SHAREZED_BOOTSTRAP")
        .map(PathBuf::from)
        .unwrap_or_else(|| store::home().join(".zshrc"))
}

/// Matched keys are dropped at capture time and never enter the log (§7.2).
fn ignore_globs() -> Vec<glob::Pattern> {
    std::env::var("SHAREZED_IGNORE")
        .unwrap_or_default()
        .split([' ', ':'])
        .filter(|p| !p.is_empty())
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect()
}

fn cursor_env() -> u64 {
    std::env::var("SHAREZED_CURSOR")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

// --- producer ---------------------------------------------------------------

fn reload(channel: &str, allow_flag: bool) -> R {
    let store = Store::open(channel)?;
    let _lock = store.lock()?;
    let boot = bootstrap();
    let content = std::fs::read(&boot)
        .map_err(|e| format!("bootstrap {}: {e} (set SHAREZED_BOOTSTRAP)", boot.display()))?;
    let hash = sha256(&content);
    let mut meta = store.meta();
    if !meta.bootstrap_hash.is_empty() && meta.bootstrap_hash != hash && !allow_flag {
        return Err(format!(
            "{} changed since the last publish.\n  review it, then: sharezed allow   (or: sharezed reload --allow)",
            boot.display()
        )
        .into());
    }

    let (s0, s1) = capture::clean_room(&boot)?;
    let desired = state::effect(&s0, &s1, &ignore_globs());
    let head = store.head();
    let changes = state::diff(&store.desired(head)?, &desired);

    meta.bootstrap = boot.to_string_lossy().into_owned();
    meta.bootstrap_hash = hash;
    store.save_meta(&meta)?;

    if changes.is_empty() {
        println!("gen {head}: nothing to publish");
        return Ok(());
    }
    validate(&changes)?;
    let seq = store.publish(&changes, &desired)?;
    println!("gen {head} → gen {seq}: {}", state::summary(&changes));
    println!("published to channel '{channel}'");
    Ok(())
}

/// Refuse to publish a payload that a bare zsh can't even parse (§7.8.1).
fn validate(changes: &[state::Change]) -> R {
    let code = payload::generate(&[(0, changes.to_vec())], &mut State::new(), &[]);
    let mut child = Command::new("zsh")
        .args(["-f", "-n"])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(code.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(format!(
            "generated payload does not parse, refusing to publish:\n{}",
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    Ok(())
}

fn allow(channel: &str) -> R {
    let store = Store::open(channel)?;
    let boot = bootstrap();
    let mut meta = store.meta();
    meta.bootstrap = boot.to_string_lossy().into_owned();
    meta.bootstrap_hash = sha256(&std::fs::read(&boot)?);
    store.save_meta(&meta)?;
    println!("trusted {}", boot.display());
    Ok(())
}

fn revert(channel: &str, seq: u64) -> R {
    let store = Store::open(channel)?;
    let _lock = store.lock()?;
    let head = store.head();
    let current = store.desired(head)?;
    let mut target = current.clone();
    for c in &store.entry(seq)? {
        match &c.old {
            Some(vals) => {
                target.insert(
                    c.key(),
                    Item {
                        attrs: c.attrs.clone(),
                        vals: vals.clone(),
                    },
                );
            }
            None => {
                target.remove(&c.key());
            }
        }
    }
    let changes = state::diff(&current, &target);
    if changes.is_empty() {
        println!("entry {seq} is already reverted");
        return Ok(());
    }
    validate(&changes)?;
    let new = store.publish(&changes, &target)?;
    println!(
        "gen {new}: reverted entry {seq} ({})",
        state::summary(&changes)
    );
    Ok(())
}

// --- consumer ---------------------------------------------------------------

fn export(channel: &str, cursor: u64) -> R {
    let store = Store::open(channel)?;
    let head = store.head();
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf)?;
    let mut ours = state::parse_wire(&buf)?;

    let mut entries = Vec::new();
    for n in cursor + 1..=head {
        entries.push((n, store.entry(n)?));
    }
    let out = payload::generate(&entries, &mut ours, &merge::default_families());
    print!("{out}");
    if entries.is_empty() {
        // Never leave the shell spinning on a cursor it can't advance.
        println!("SHAREZED_CURSOR={head}");
    }
    Ok(())
}

fn hook(channel: &str) -> R {
    let store = Store::open(channel)?;
    let bin = std::env::current_exe()?;
    print!(
        "{}",
        include_str!("hook.zsh")
            .replace("@BIN@", &payload::zq(&bin.to_string_lossy()))
            .replace("@CHANNEL@", &payload::zq(channel))
            .replace("@HEAD@", &payload::zq(&store.head_path().to_string_lossy()))
    );
    Ok(())
}

// --- inspection -------------------------------------------------------------

fn status(channel: &str) -> R {
    let store = Store::open(channel)?;
    let (head, cursor) = (store.head(), cursor_env());
    let pending = head.saturating_sub(cursor);
    println!(
        "channel {channel}: cursor {cursor}, head {head}{}",
        match pending {
            0 => " (up to date)".into(),
            n => format!(" ({n} pending)"),
        }
    );
    let conflicts = std::env::var("SHAREZED_CONFLICTS").unwrap_or_default();
    if !conflicts.is_empty() {
        println!(
            "conflicts (local edits kept): {}",
            conflicts.replace(':', ", ")
        );
    }
    if std::env::var_os("SHAREZED_DISABLE").is_some() {
        println!("DISABLED in this shell (SHAREZED_DISABLE is set)");
    }
    if cursor == 0 && std::env::var_os("SHAREZED_HEAD").is_none() {
        println!("note: no hook in this shell — eval \"$(sharezed hook zsh)\" in .zshrc");
    }
    Ok(())
}

fn describe(c: &state::Change) -> String {
    let show = |v: &Vec<String>| {
        // One line per change: function bodies are multi-line, and `diff` is
        // what you read before trusting an entry (§7.7).
        let s = v.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
        if s.len() > 60 {
            format!("{}…", &s[..s.floor_char_boundary(60)])
        } else {
            s
        }
    };
    let (sign, detail) = match (&c.old, &c.new) {
        (None, Some(n)) => ("+", show(n)),
        (Some(o), Some(n)) => ("~", format!("{} → {}", show(o), show(n))),
        (Some(o), None) => ("-", show(o)),
        (None, None) => ("?", String::new()),
    };
    format!("  {sign} {:<7} {:<24} {detail}", c.kind.as_str(), c.name)
}

fn diff(channel: &str, seq: Option<u64>) -> R {
    let store = Store::open(channel)?;
    let seq = seq.unwrap_or_else(|| store.head());
    if seq == 0 {
        println!("nothing published on channel {channel}");
        return Ok(());
    }
    let changes = store.entry(seq)?;
    println!(
        "gen {seq} (channel {channel}): {}",
        state::summary(&changes)
    );
    for c in &changes {
        println!("{}", describe(c));
    }
    Ok(())
}

fn log(channel: &str) -> R {
    let store = Store::open(channel)?;
    for seq in 1..=store.head() {
        println!("{seq:>6}  {}", state::summary(&store.entry(seq)?));
    }
    Ok(())
}

fn live_path() -> Vec<String> {
    // ponytail: the scalar side is good enough here — this is a report, and an
    // interactive invocation has no way to hand us the array.
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .map(str::to_string)
        .collect()
}

fn path_explain(channel: &str) -> R {
    let store = Store::open(channel)?;
    let head = store.head();
    let key = (Kind::Array, "path".to_string());
    let published = store
        .desired(head)?
        .get(&key)
        .map(|i| i.vals.clone())
        .unwrap_or_default();
    let ours = live_path();
    println!("channel {channel}, gen {head}");
    if published.is_empty() {
        println!("  path is not managed on this channel");
        return Ok(());
    }
    let m = merge::merge(&published, &ours, &published, &merge::default_families());
    let pubset: Vec<String> = published.iter().map(|p| merge::norm(p)).collect();
    for (i, e) in m.list.iter().enumerate() {
        let origin = if pubset.contains(&merge::norm(e)) {
            "published"
        } else {
            "local"
        };
        println!("  {:>2}. {e:<44} {origin}", i + 1);
    }
    for n in &m.notes {
        println!("  note: {n}");
    }
    Ok(())
}

fn doctor(channel: &str, prune_missing: bool) -> R {
    let store = Store::open(channel)?;
    let mut warn = 0;
    let mut check = |ok: bool, msg: &str| {
        println!("{} {msg}", if ok { "ok  " } else { "WARN" });
        warn += u32::from(!ok);
    };

    let boot = bootstrap();
    let rc = std::fs::read_to_string(&boot).unwrap_or_default();
    let pos = |needle: &str| rc.lines().position(|l| l.contains(needle));
    match (pos("sharezed hook"), pos("direnv hook")) {
        (None, _) => check(
            false,
            &format!("no `sharezed hook zsh` in {}", boot.display()),
        ),
        (Some(s), Some(d)) => check(
            s < d,
            "direnv hook must come after sharezed's — directory scope has to win (§12)",
        ),
        (Some(_), None) => check(true, "hook installed"),
    }
    check(
        store.meta().bootstrap_hash == sha256(&std::fs::read(&boot).unwrap_or_default()),
        &format!("{} matches the last published generation", boot.display()),
    );
    let cursor = cursor_env();
    check(
        cursor == store.head(),
        &format!("cursor {cursor} vs head {} in this shell", store.head()),
    );

    if prune_missing {
        for p in live_path()
            .iter()
            .filter(|p| !p.is_empty() && !PathBuf::from(p).is_dir())
        {
            check(false, &format!("PATH entry does not exist: {p}"));
        }
    }

    // Capture twice: a bootstrap that branches on the clock or on a file it
    // rewrites (compinit's `.zcompdump(#qN.mh+24)` does both) is not a pure
    // function of its text, and every flip publishes a phantom generation.
    // Only catches a flip whose trigger is armed right now.
    let ignore = ignore_globs();
    let capture_twice = || -> R<state::State> {
        let (s0, s1) = capture::clean_room(&boot)?;
        Ok(state::effect(&s0, &s1, &ignore))
    };
    let flap = state::diff(&capture_twice()?, &capture_twice()?);
    check(
        flap.is_empty(),
        &format!(
            "bootstrap is reproducible ({} key(s) differ across two captures)",
            flap.len()
        ),
    );
    for c in &flap {
        println!("       {} {}", c.kind.as_str(), c.name);
    }

    if warn > 0 {
        return Err(format!("{warn} warning(s)").into());
    }
    Ok(())
}

fn sha256(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    /// M0's exit criterion, end to end: capture a bootstrap in a clean room and
    /// see exactly what it added. Needs zsh; skipped if absent.
    #[test]
    fn clean_room_captures_what_the_bootstrap_did() {
        if Command::new("zsh").arg("-fc").arg("true").status().is_err() {
            return;
        }
        let boot = std::env::temp_dir().join(format!("sharezed-test-{}.zsh", std::process::id()));
        std::fs::write(
            &boot,
            "export EDITOR=acme\nMYTOKEN=sekrit\nalias gs='git status'\n\
             work() { print \"working in $1\" }\ntypeset -a mylist=(a b)\npath=(/opt/x $path)\n",
        )
        .unwrap();
        let (s0, s1) = capture::clean_room(&boot).unwrap();
        let d = state::effect(&s0, &s1, &[glob::Pattern::new("*TOKEN*").unwrap()]);
        let _ = std::fs::remove_file(&boot);

        let get = |k: Kind, n: &str| d.get(&(k, n.to_string())).map(|i| i.vals.clone());
        assert_eq!(get(Kind::Scalar, "EDITOR"), Some(vec!["acme".into()]));
        assert_eq!(get(Kind::Alias, "gs"), Some(vec!["git status".into()]));
        // zsh re-indents function bodies; compare on content, not whitespace.
        assert!(get(Kind::Func, "work").unwrap()[0].contains("working in $1"));
        assert_eq!(
            get(Kind::Array, "mylist"),
            Some(vec!["a".into(), "b".into()])
        );
        assert_eq!(
            get(Kind::Scalar, "MYTOKEN"),
            None,
            "SHAREZED_IGNORE must drop it"
        );
        assert_eq!(
            get(Kind::Scalar, "PATH"),
            None,
            "tied scalar twin is suppressed (§5.4c)"
        );
        assert_eq!(
            get(Kind::Array, "path")
                .unwrap()
                .first()
                .map(String::as_str),
            Some("/opt/x"),
            "the array side carries PATH"
        );
        assert!(
            get(Kind::Scalar, "PWD").is_none(),
            "per-shell state must never be captured"
        );
        assert!(
            !d.contains_key(&(Kind::Func, "_sz_dump".into())),
            "harness must not leak"
        );
    }
}
