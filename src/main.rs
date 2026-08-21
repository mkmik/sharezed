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
        /// Capture even if no tracked file or command changed.
        #[arg(long)]
        force: bool,
        /// Print nothing on success. Errors still go to stderr.
        #[arg(long)]
        silent: bool,
        /// Report only: exit 1 if a capture would have something to do.
        #[arg(long)]
        check: bool,
        /// Capture but publish nothing: show the diff and exit 1 if there is
        /// something to publish.
        #[arg(long, conflicts_with = "check")]
        dry_run: bool,
        /// Publish nothing, but if there is nothing to publish, record the
        /// fingerprints anyway — one capture, so nothing slips in between.
        #[arg(long, conflicts_with_all = ["check", "dry_run"])]
        if_noop: bool,
        /// Summary only: how much would be published, not what.
        #[arg(short = 'p', long)]
        plain: bool,
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
        Cmd::Reload {
            force,
            silent,
            check,
            dry_run,
            if_noop,
            plain,
        } => reload(ch, *force, *silent, *check, *dry_run, *if_noop, *plain),
        Cmd::Status => status(ch),
        Cmd::Diff { seq } => diff(ch, *seq),
        Cmd::Log => log(ch),
        Cmd::Export { cursor, .. } => export(ch, *cursor),
        Cmd::Hook { .. } => hook(ch),
        Cmd::Revert { seq } => revert(ch, *seq),
        Cmd::Path { .. } => path_explain(ch),
        Cmd::Doctor { prune_missing } => doctor(ch, *prune_missing),
    }
}

// --- config -----------------------------------------------------------------
// ponytail: env vars, no config file. The PRD's TOML (§8.4, §8.7) buys a parser
// dependency for values nobody has needed to change yet.

/// `None` means the startup files zsh itself would run, in order — which is
/// the only way to see `~/.zshenv`, where a PATH entry like `~/.cargo/bin`
/// usually comes from. Set SHAREZED_BOOTSTRAP to capture one file instead.
fn bootstrap() -> Option<PathBuf> {
    std::env::var_os("SHAREZED_BOOTSTRAP").map(PathBuf::from)
}

/// Where the hook belongs, regardless of what is being captured.
fn zshrc() -> PathBuf {
    std::env::var_os("ZDOTDIR")
        .map(PathBuf::from)
        .unwrap_or_else(store::home)
        .join(".zshrc")
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

/// PATH-style elements under `$TMPDIR` never reach the log: anything there is
/// per-session by construction.
fn volatile_globs() -> Vec<glob::Pattern> {
    match std::env::var("TMPDIR") {
        // An empty TMPDIR would build the pattern `/*`, which drops every
        // absolute path there is.
        Ok(t) if !t.is_empty() => glob::Pattern::new(&format!("{}/*", merge::norm(&t)))
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

fn cursor_env() -> u64 {
    std::env::var("SHAREZED_CURSOR")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

// --- producer ---------------------------------------------------------------

fn reload(
    channel: &str,
    force: bool,
    silent: bool,
    check: bool,
    dry_run: bool,
    if_noop: bool,
    plain: bool,
) -> R {
    // Errors keep going to stderr: silent is about routine chatter on a timer,
    // not about hiding a broken bootstrap.
    let say = |msg: String| {
        if !silent {
            println!("{msg}");
        }
    };
    let store = Store::open(channel)?;
    if check {
        // Deliberately before the lock: this runs on every prompt in every
        // shell, and an exclusive flock there would serialize all of them.
        if let Some(p) = stale_dep(&store.meta()) {
            say(format!("changed: {p}"));
            std::process::exit(1);
        }
        return Ok(());
    }
    let _lock = store.lock()?;
    let boot = bootstrap();
    if let Some(b) = &boot
        && !b.is_file()
    {
        return Err(format!("bootstrap {} is not a file", b.display()).into());
    }

    let meta = store.meta();
    // Re-hashing what the last capture recorded needs no shell at all, so this
    // is milliseconds against a second-plus. Adding a new `source` line or a
    // new command means editing a file that is already tracked, so a change
    // that matters always shows up here first. `--force` is for the rest: a
    // bootstrap that reads a file it never sources, or a change to
    // SHAREZED_IGNORE, moves no fingerprint.
    if !force && !meta.sources.is_empty() {
        match stale_dep(&meta) {
            None => {
                say(format!(
                    "gen {}: {} and {} unchanged",
                    store.head(),
                    plural(meta.sources.len(), "file"),
                    plural(meta.commands.len(), "command")
                ));
                return Ok(());
            }
            Some(p) => say(format!("changed: {p}")),
        }
    }

    // The prompt runs this on every keypress while the files are dirty, and a
    // delta nobody has published yet keeps them dirty. Nothing has moved since
    // the capture that found it, so its answer still stands — and answering
    // from memory is 2ms against a second.
    let digest = dep_digest(&meta);
    if if_noop && !force && meta.stalled == digest && !digest.is_empty() {
        say(format!(
            "gen {}: still something to publish, unchanged since the last capture",
            store.head()
        ));
        std::process::exit(1);
    }

    let cap = capture::clean_room(boot.as_deref())?;
    // A fresh capture answers for itself, so it clears any memo.
    let fresh = store::Meta {
        sources: hash_sources(&cap.sources),
        commands: fingerprints(&cap.commands),
        stalled: String::new(),
    };

    let mut desired = state::effect(&cap.s0, &cap.s1, &ignore_globs());
    state::drop_volatile(&mut desired, &volatile_globs());
    let head = store.head();
    let changes = state::diff(&store.desired(head)?, &desired);

    if changes.is_empty() {
        // The fingerprints are the only thing that moved, so recording them is
        // the whole reload — and the prompt nag goes quiet. A dry run doesn't:
        // it must stay repeatable and answer for a capture, not perform one.
        if !dry_run {
            store.save_meta(&fresh)?;
        }
        say(format!("gen {head}: nothing to publish"));
        return Ok(());
    }
    validate(&changes)?;
    // Fingerprints stay stale here on purpose: nothing was published, so the
    // nag has to survive — there is a real delta waiting for a human.
    if dry_run || if_noop {
        if if_noop {
            store.save_meta(&store::Meta {
                stalled: digest,
                ..meta
            })?;
        }
        say(format!(
            "gen {head}: would publish {}",
            state::summary(&changes)
        ));
        // What `sharezed diff` would show after the reload — deciding whether to
        // publish means reading the keys, not counting them.
        if !plain {
            for c in &changes {
                say(describe(c));
            }
        }
        std::process::exit(1);
    }
    let seq = store.publish(&changes, &desired)?;
    // After the publish, never before: a fingerprint recorded for a generation
    // that failed to land would clear the nag and never be retried.
    store.save_meta(&fresh)?;
    say(format!(
        "gen {head} → gen {seq}: {}",
        state::summary(&changes)
    ));
    say(format!("published to channel '{channel}'"));
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

/// The first tracked file or command whose fingerprint no longer matches.
fn stale_dep(meta: &store::Meta) -> Option<String> {
    let file_now = |p: &str| Some(sha256(&std::fs::read(p).ok()?));
    (meta
        .sources
        .iter()
        .find(|(p, h)| file_now(p).as_ref() != Some(*h)))
    .or_else(|| {
        meta.commands
            .iter()
            .find(|(p, f)| fingerprint(Path::new(p)).as_ref() != Some(*f))
    })
    .map(|(p, _)| p.clone())
}

/// The tracked files and commands as they are *now*, in one line. `stale_dep`
/// asks whether they still match the last publish; this is what lets a caller
/// ask whether they still match the last capture. Empty when nothing is
/// tracked yet, which must never count as a match.
fn dep_digest(meta: &store::Meta) -> String {
    if meta.sources.is_empty() && meta.commands.is_empty() {
        return String::new();
    }
    let mut buf = String::new();
    for p in meta.sources.keys() {
        let h = std::fs::read(p).map(|b| sha256(&b)).unwrap_or_default();
        buf.push_str(&format!("{p}\0{h}\n"));
    }
    for p in meta.commands.keys() {
        buf.push_str(&format!(
            "{p}\0{}\n",
            fingerprint(Path::new(p)).unwrap_or_default()
        ));
    }
    sha256(buf.as_bytes())
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

/// `~ path` changed, `+ path` newly sourced, `- path` no longer sourced.
fn changed_sources(
    was: &std::collections::BTreeMap<String, String>,
    now: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    let mut out: Vec<String> = now
        .iter()
        .filter(|(p, h)| was.get(*p) != Some(*h))
        .map(|(p, _)| format!("{} {p}", if was.contains_key(p) { "~" } else { "+" }))
        .collect();
    out.extend(
        was.keys()
            .filter(|p| !now.contains_key(*p))
            .map(|p| format!("- {p}")),
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
        // A prompt is mostly escape sequences: printed raw they would repaint
        // the terminal instead of describing the change. `\e` is the spelling
        // the zshrc that produced them uses.
        let s: String = s
            .chars()
            .flat_map(|c| match c {
                '\x1b' => vec!['\\', 'e'],
                c if c.is_control() => c.escape_debug().collect(),
                c => vec![c],
            })
            .collect();
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

/// Every line states what *is*, never what ought to be — "ok" next to "must
/// come after" leaves you unable to tell whether it did. `note` is for things
/// that are true, unactionable, and permanent; only `warn` sets the exit code,
/// so a clean shell can rely on it.
fn doctor(channel: &str, prune_missing: bool) -> R {
    let store = Store::open(channel)?;
    let head = store.head();
    let mut lines: Vec<(&str, String)> = Vec::new();

    // No direnv ordering check: its zsh hook *prepends* itself to
    // precmd_functions, so it runs first whatever order the two evals appear
    // in. §12's advice to place sharezed's line first is a no-op, and warning
    // about it was warning about nothing.
    let rc_path = zshrc();
    let rc = std::fs::read_to_string(&rc_path).unwrap_or_default();
    lines.push(match rc.lines().any(|l| l.contains("sharezed hook")) {
        true => ("ok", format!("hook installed in {}", rc_path.display())),
        false => (
            "warn",
            format!("no `sharezed hook zsh` in {}", rc_path.display()),
        ),
    });

    let cursor = cursor_env();
    lines.push(match () {
        _ if std::env::var_os("SHAREZED_HEAD").is_none() => (
            "note",
            "this shell has no hook, so it will not converge on its own".into(),
        ),
        _ if cursor == head => ("ok", format!("this shell is up to date (gen {head})")),
        _ => (
            "warn",
            format!("this shell is at gen {cursor}, {head} is published"),
        ),
    });

    // Capture twice: a bootstrap that branches on the clock or on a file it
    // rewrites (compinit's `.zcompdump(#qN.mh+24)` does both) is not a pure
    // function of its text, and every flip publishes a phantom generation.
    // Only catches a flip whose trigger is armed right now.
    let boot = bootstrap();
    let (first, second) = (
        capture::clean_room(boot.as_deref())?,
        capture::clean_room(boot.as_deref())?,
    );
    let (ignore, volatile) = (ignore_globs(), volatile_globs());
    let effect = |c: &capture::Capture| {
        let mut e = state::effect(&c.s0, &c.s1, &ignore);
        state::drop_volatile(&mut e, &volatile);
        e
    };
    let flap = state::diff(&effect(&first), &effect(&second));
    if flap.is_empty() {
        lines.push(("ok", "capture is reproducible".into()));
    } else {
        lines.push((
            "warn",
            format!(
                "capture is not reproducible: {} differ between two runs",
                plural(flap.len(), "key")
            ),
        ));
        lines.extend(
            flap.iter()
                .map(|c| ("", format!("{} {}", c.kind.as_str(), c.name))),
        );
    }

    let meta = store.meta();
    for (what, was, now, total) in [
        (
            "sourced file",
            meta.sources,
            hash_sources(&first.sources),
            first.sources.len(),
        ),
        (
            "command",
            meta.commands,
            fingerprints(&first.commands),
            first.commands.len(),
        ),
    ] {
        let changed = changed_sources(&was, &now);
        if changed.is_empty() {
            lines.push((
                "ok",
                format!("{} unchanged since gen {head}", plural(total, what)),
            ));
        } else {
            lines.push((
                "warn",
                format!(
                    "{} changed since gen {head} — run `sharezed reload`",
                    plural(changed.len(), what)
                ),
            ));
            lines.extend(changed.iter().map(|c| ("", c.clone())));
        }
    }

    if prune_missing {
        for p in live_path()
            .iter()
            .filter(|p| !p.is_empty() && !PathBuf::from(p).is_dir())
        {
            lines.push(("warn", format!("PATH entry no longer exists: {p}")));
        }
    }

    for (level, msg) in &lines {
        match *level {
            "" => println!("       {msg}"),
            l => println!("{l:<5}  {msg}"),
        }
    }
    match lines.iter().filter(|(l, _)| *l == "warn").count() {
        0 => Ok(()),
        n => Err(plural(n, "warning").into()),
    }
}

fn plural(n: usize, s: &str) -> String {
    format!("{n} {s}{}", if n == 1 { "" } else { "s" })
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

    /// Every shell-level test needs one; a dev machine without zsh should skip
    /// rather than fail. CI installs it, so there they all run.
    fn has_zsh() -> bool {
        Command::new("zsh").arg("-fc").arg("true").status().is_ok()
    }

    /// A channel with nothing published must not make every prompt print an
    /// error. Shell-level, so no amount of Rust testing would have caught it.
    #[test]
    fn hook_is_silent_when_head_is_missing() {
        if !has_zsh() {
            return;
        }
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

    /// The nag is a three-way decision now — settled, refused, or opted out —
    /// and an `&&`/`||` chain is exactly the thing that silently inverts. Stub
    /// the binary so this is about the branch, not about capture.
    #[test]
    fn precmd_settles_before_it_nags() {
        if !has_zsh() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("sharezed-hook-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (bin, calls) = (dir.join("stub"), dir.join("calls"));
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n\
                 *--check*) exit ${{STUB_CHECK:-0}} ;;\n\
                 *--if-noop*) exit ${{STUB_SETTLE:-0}} ;;\nesac\nexit 0\n",
                calls.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let hook = include_str!("hook.zsh")
            .replace("@BIN@", &payload::zq(&bin.to_string_lossy()))
            .replace("@CHANNEL@", "'user'")
            .replace(
                "@HEAD@",
                &payload::zq(&dir.join("nohead").to_string_lossy()),
            );

        // (check, settle, SHAREZED_NO_SETTLE) -> (RPROMPT, what was called)
        let run = |check: &str, settle: &str, no_settle: &str| -> (String, String) {
            let _ = std::fs::remove_file(&calls);
            let out = Command::new("zsh")
                .args(["-f", "-c"])
                .arg(format!(
                    "{hook}\nRPROMPT=keepme\n_sharezed_precmd\nprint -r -- $RPROMPT"
                ))
                .env("STUB_CHECK", check)
                .env("STUB_SETTLE", settle)
                .env("SHAREZED_NO_SETTLE", no_settle)
                .env_remove("SHAREZED_AUTORELOAD")
                .env_remove("SHAREZED_NO_NOTIFY")
                .env_remove("SHAREZED_DISABLE")
                .output()
                .unwrap();
            (
                String::from_utf8_lossy(&out.stdout).into_owned(),
                std::fs::read_to_string(&calls).unwrap_or_default(),
            )
        };
        let nagged = |s: &str| s.contains("sharezed reload");

        let (rprompt, calls) = run("0", "0", "");
        assert!(!nagged(&rprompt), "clean: {rprompt:?}");
        assert!(!calls.contains("--if-noop"), "clean: nothing to settle");

        let (rprompt, calls) = run("1", "0", "");
        assert!(!nagged(&rprompt), "settled, so no nag: {rprompt:?}");
        assert!(calls.contains("--if-noop"), "dirty: settle it");

        let (rprompt, _) = run("1", "1", "");
        assert!(nagged(&rprompt), "a real delta must still nag: {rprompt:?}");

        let (rprompt, calls) = run("1", "0", "1");
        assert!(nagged(&rprompt), "opted out, so nag as before: {rprompt:?}");
        assert!(!calls.contains("--if-noop"), "opted out: no capture");

        assert!(
            run("0", "0", "").0.starts_with("keepme"),
            "the rest of RPROMPT is not ours to touch"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// M0's exit criterion, end to end: capture a bootstrap in a clean room and
    /// see exactly what it added. Needs zsh; skipped if absent.
    #[test]
    fn clean_room_captures_what_the_bootstrap_did() {
        if !has_zsh() {
            return;
        }
        let boot = std::env::temp_dir().join(format!("sharezed-test-{}.zsh", std::process::id()));
        std::fs::write(
            &boot,
            "export EDITOR=acme\nMYTOKEN=sekrit\nalias gs='git status'\n\
             work() { print \"working in $1\" }\ntypeset -a mylist=(a b)\npath=(/opt/x $path)\n\
             export SHAREZED_NOTIFY=1\nexport SHAREZED_CURSOR=99\n\
             _sharezed_precmd() { : }\n",
        )
        .unwrap();
        let cap = capture::clean_room(Some(&boot)).unwrap();
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
