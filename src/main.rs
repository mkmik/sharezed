//! sharezed — direnv for everything a shell knows. See docs/PRD.md.

mod capture;
mod merge;
mod payload;
mod state;
mod store;

use clap::{Parser, Subcommand};
use state::{Item, Kind, State};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
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
    if !boot.is_file() {
        return Err(format!(
            "bootstrap {} is not a file (set SHAREZED_BOOTSTRAP)",
            boot.display()
        )
        .into());
    }

    // The file list comes out of the capture itself, so the trust gate has to
    // run after it. Capture is not the dangerous step — publishing is.
    let cap = capture::clean_room(&boot)?;
    let sources = hash_sources(&cap.sources);
    let mut meta = store.meta();
    let changed = changed_sources(&meta.sources, &sources);
    if !meta.sources.is_empty() && !changed.is_empty() && !allow_flag {
        return Err(format!(
            "{} sourced file(s) changed since the last publish:\n{}\n  review, then: sharezed allow   (or: sharezed reload --allow)",
            changed.len(),
            changed.join("\n")
        )
        .into());
    }

    let desired = state::effect(&cap.s0, &cap.s1, &ignore_globs());
    let head = store.head();
    let changes = state::diff(&store.desired(head)?, &desired);

    meta.bootstrap = boot.to_string_lossy().into_owned();
    meta.sources = sources;
    meta.commands = fingerprints(&cap.commands);
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

/// Trust the current content of every file the bootstrap sources. Needs its own
/// capture to learn the file list — rare command, so the second clean-room run
/// is cheaper than carrying pending state between invocations.
fn allow(channel: &str) -> R {
    let store = Store::open(channel)?;
    let boot = bootstrap();
    let mut meta = store.meta();
    meta.bootstrap = boot.to_string_lossy().into_owned();
    let cap = capture::clean_room(&boot)?;
    meta.sources = hash_sources(&cap.sources);
    meta.commands = fingerprints(&cap.commands);
    let n = meta.sources.len();
    store.save_meta(&meta)?;
    println!("trusted {} and {} sourced file(s)", boot.display(), n - 1);
    Ok(())
}

/// sha256 every sourced file. Process substitutions (`. <(cmd)`) show up as
/// `/dev/fd/N` — unhashable, and re-reading that path in *this* process would
/// name one of our own descriptors, so they are skipped, not guessed at.
fn hash_sources(files: &[PathBuf]) -> std::collections::BTreeMap<String, String> {
    files
        .iter()
        .filter(|f| !f.starts_with("/dev/") && f.is_file())
        .filter_map(|f| {
            Some((
                f.to_string_lossy().into_owned(),
                sha256(&std::fs::read(f).ok()?),
            ))
        })
        .collect()
}

/// Under this, a file is a script: its content is what matters and reading it
/// is free. Over it, a compiled binary — content-hashing your PATH is 195 MB
/// and 0.6s per reload for no extra signal.
const HASH_BELOW: u64 = 16 * 1024;

/// Metadata is enough to notice an upgrade, and package managers put the
/// version straight into the symlink target
/// (`flux -> ../Cellar/flux/2.1.0/bin/flux`).
fn fingerprint(p: &Path) -> Option<String> {
    let md = std::fs::metadata(p).ok()?;
    if md.len() < HASH_BELOW {
        return Some(sha256(&std::fs::read(p).ok()?));
    }
    let target = std::fs::read_link(p).unwrap_or_default();
    let mtime = md
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(format!("{} {} {mtime}", target.display(), md.len()))
}

fn fingerprints(files: &[PathBuf]) -> std::collections::BTreeMap<String, String> {
    files
        .iter()
        .filter_map(|f| Some((f.to_string_lossy().into_owned(), fingerprint(f)?)))
        .collect()
}

fn unhashable(files: &[PathBuf]) -> usize {
    files
        .iter()
        .filter(|f| f.starts_with("/dev/") || !f.is_file())
        .count()
}

fn verdict(n: usize, changed: usize, what: &str) -> String {
    match changed {
        0 => format!("{n} {what}(s) match the last published generation"),
        c => format!("{c} of {n} {what}(s) changed since the last publish"),
    }
}

/// `~ path` changed, `+ path` newly sourced, `- path` no longer sourced.
fn changed_sources(
    was: &std::collections::BTreeMap<String, String>,
    now: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    let mut out: Vec<String> = now
        .iter()
        .filter(|(p, h)| was.get(*p) != Some(*h))
        .map(|(p, _)| format!("    {} {p}", if was.contains_key(p) { "~" } else { "+" }))
        .collect();
    out.extend(
        was.keys()
            .filter(|p| !now.contains_key(*p))
            .map(|p| format!("    - {p}")),
    );
    out
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
    // A 40-element PATH truncated to 60 chars is two identical-looking
    // prefixes, so lists get an element-wise diff instead.
    if let (Some(o), Some(n)) = (&c.old, &c.new)
        && matches!(c.kind, Kind::Array | Kind::Assoc)
    {
        let mut out = format!(
            "  ~ {:<7} {:<24} {} → {} elements",
            c.kind.as_str(),
            c.name,
            o.len(),
            n.len()
        );
        for line in element_diff(o, n) {
            out.push('\n');
            out.push_str(&line);
        }
        return out;
    }
    let (sign, detail) = match (&c.old, &c.new) {
        (None, Some(n)) => ("+", show(n)),
        (Some(o), Some(n)) => ("~", format!("{} → {}", show(o), show(n))),
        (Some(o), None) => ("-", show(o)),
        (None, None) => ("?", String::new()),
    };
    format!("  {sign} {:<7} {:<24} {detail}", c.kind.as_str(), c.name)
}

/// Removals with their old position, then additions with their new one. A
/// moved element shows as both, which is the truth for a priority-ordered
/// list: its index *is* its meaning.
fn element_diff(old: &[String], new: &[String]) -> Vec<String> {
    let matched = merge::lcs(old, new);
    let (kept_old, kept_new): (std::collections::HashSet<_>, std::collections::HashSet<_>) =
        matched.into_iter().unzip();
    let pick = |v: &[String], kept: &std::collections::HashSet<usize>, sign: char| -> Vec<String> {
        v.iter()
            .enumerate()
            .filter(|(i, _)| !kept.contains(i))
            .map(|(i, e)| format!("      {sign} {:>3}  {e}", i + 1))
            .collect()
    };
    let mut out = pick(old, &kept_old, '-');
    out.extend(pick(new, &kept_new, '+'));
    if out.is_empty() {
        out.push("        (same elements; attributes changed)".into());
    }
    out
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
    let capture_twice = || -> R<capture::Capture> { capture::clean_room(&boot) };
    let (first, second) = (capture_twice()?, capture_twice()?);
    let flap = state::diff(
        &state::effect(&first.s0, &first.s1, &ignore),
        &state::effect(&second.s0, &second.s1, &ignore),
    );
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

    // Every sourced file is code entering 30 shells, so the trust gate covers
    // all of them, not just the one named in SHAREZED_BOOTSTRAP.
    let changed = changed_sources(&store.meta().sources, &hash_sources(&first.sources));
    check(
        changed.is_empty(),
        &verdict(first.sources.len(), changed.len(), "sourced file"),
    );
    for c in &changed {
        println!("   {c}");
    }
    let stale = changed_sources(&store.meta().commands, &fingerprints(&first.commands));
    check(
        stale.is_empty(),
        &verdict(first.commands.len(), stale.len(), "traced command"),
    );
    for s in &stale {
        println!("   {s}");
    }
    if !stale.is_empty() {
        println!("       run `sharezed reload` to pick up what they now produce");
    }

    let opaque = unhashable(&first.sources);
    if opaque > 0 {
        check(
            false,
            &format!(
                "{opaque} sourced file(s) cannot be hashed (process substitution) — unreviewable code"
            ),
        );
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

    #[test]
    fn list_changes_render_element_wise() {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // /c moves to the front, /b goes away, /new arrives
        let d = element_diff(&v(&["/a", "/b", "/c"]), &v(&["/c", "/new", "/a"]));
        assert_eq!(
            d,
            [
                "      -   1  /a",
                "      -   2  /b",
                "      +   2  /new",
                "      +   3  /a",
            ],
            "a move is a removal plus an addition: in a priority-ordered list the index is the meaning"
        );
        assert!(element_diff(&v(&["/a"]), &v(&["/a"]))[0].contains("attributes changed"));
    }

    /// A channel with nothing published must not make every prompt print an
    /// error. Shell-level, so no amount of Rust testing would have caught it.
    #[test]
    fn hook_is_silent_when_head_is_missing() {
        let head = std::env::temp_dir().join("sharezed-no-such-head");
        let _ = std::fs::remove_file(&head);
        let hook = include_str!("hook.zsh")
            .replace("@BIN@", "'/nonexistent'")
            .replace("@CHANNEL@", "'user'")
            .replace("@HEAD@", &payload::zq(&head.to_string_lossy()));
        let out = Command::new("zsh")
            .args(["-f", "-c"])
            .arg(format!("{hook}\n_sharezed_precmd\n_sharezed_precmd"))
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "",
            "hook must be silent when the channel is empty"
        );
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
        let cap = capture::clean_room(&boot).unwrap();
        let d = state::effect(&cap.s0, &cap.s1, &[glob::Pattern::new("*TOKEN*").unwrap()]);
        assert!(
            cap.sources.contains(&boot),
            "SOURCE_TRACE must name the bootstrap itself: {:?}",
            cap.sources
        );
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
