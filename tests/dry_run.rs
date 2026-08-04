//! `reload --dry-run` is only useful if its exit code is trustworthy and it
//! publishes nothing; `--if-noop` adds "and it quiets the nag when, and only
//! when, there was nothing to publish". End-to-end properties, so this drives
//! the binary.

use std::path::Path;
use std::process::Command;

fn sharezed(state: &Path, boot: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sharezed"))
        .args(["reload"])
        .args(args)
        .env("XDG_STATE_HOME", state)
        .env("SHAREZED_BOOTSTRAP", boot)
        .output()
        .expect("run sharezed");
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn has_zsh() -> bool {
    Command::new("zsh").arg("-fc").arg("true").status().is_ok()
}

fn head(state: &Path) -> String {
    std::fs::read_to_string(state.join("sharezed/user/head")).unwrap()
}

#[test]
fn dry_run_reports_without_publishing() {
    if !has_zsh() {
        return;
    }
    let tmp = std::env::temp_dir().join(format!("sharezed-dry-{}", std::process::id()));
    let (state, boot) = (tmp.join("state"), tmp.join("boot.zsh"));
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(&boot, "export DRY_PROBE=1\n").unwrap();
    assert_eq!(sharezed(&state, &boot, &[]).0, 0, "first publish");

    std::fs::write(&boot, "export DRY_PROBE=2\n").unwrap();
    let (code, out) = sharezed(&state, &boot, &["--dry-run"]);
    assert_eq!(code, 1, "something to publish → exit 1: {out}");
    assert!(out.contains("would publish"), "{out}");

    // Nothing was published, and no fingerprint was recorded — so a second dry
    // run must say exactly the same thing rather than go quiet.
    assert_eq!(sharezed(&state, &boot, &["--dry-run"]), (code, out));

    // The case this exists for: a tracked file changed, but the capture yields
    // no delta. `--check` says "changed"; only a dry run can say "nothing".
    assert_eq!(sharezed(&state, &boot, &[]).0, 0, "publish the edit");
    std::fs::write(&boot, "export DRY_PROBE=2 \n").unwrap();
    let (code, out) = sharezed(&state, &boot, &["--dry-run"]);
    assert_eq!(code, 0, "nothing to publish → exit 0: {out}");
    assert!(out.contains("nothing to publish"), "{out}");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn if_noop_settles_the_harmless_case_and_leaves_the_real_one() {
    if !has_zsh() {
        return;
    }
    let tmp = std::env::temp_dir().join(format!("sharezed-noop-{}", std::process::id()));
    let (state, boot) = (tmp.join("state"), tmp.join("boot.zsh"));
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(&boot, "export NOOP_PROBE=1\n").unwrap();
    assert_eq!(sharezed(&state, &boot, &[]).0, 0, "first publish");
    let published = head(&state);

    // The case it exists for: a tracked file moved, the capture publishes
    // nothing. --check nags, --if-noop settles it without publishing.
    std::fs::write(&boot, "export NOOP_PROBE=1 \n").unwrap();
    assert_eq!(sharezed(&state, &boot, &["--check"]).0, 1, "nag is on");
    let (code, out) = sharezed(&state, &boot, &["--if-noop"]);
    assert_eq!(code, 0, "no delta → exit 0: {out}");
    assert!(out.contains("nothing to publish"), "{out}");
    assert_eq!(head(&state), published, "nothing may be published");
    assert_eq!(sharezed(&state, &boot, &["--check"]).0, 0, "nag is off");

    // A real delta: publish nothing, and keep nagging about it.
    std::fs::write(&boot, "export NOOP_PROBE=2\n").unwrap();
    let (code, out) = sharezed(&state, &boot, &["--if-noop"]);
    assert_eq!(code, 1, "delta → exit 1: {out}");
    assert!(out.contains("would publish"), "{out}");
    assert_eq!(head(&state), published, "nothing may be published");
    assert_eq!(
        sharezed(&state, &boot, &["--check"]).0,
        1,
        "a refused capture must leave the nag on"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// The prompt calls this on every keypress while the files are dirty, so the
/// answer it just computed has to come back from memory — a second-long capture
/// per prompt is the difference between usable and not.
#[test]
fn if_noop_recaptures_only_when_something_moved() {
    if !has_zsh() {
        return;
    }
    let tmp = std::env::temp_dir().join(format!("sharezed-memo-{}", std::process::id()));
    let (state, boot) = (tmp.join("state"), tmp.join("boot.zsh"));
    let tally = tmp.join("captures");
    std::fs::create_dir_all(&state).unwrap();
    // The bootstrap is only ever sourced by a capture, so it can count them.
    let write_boot = |body: &str| {
        let line = format!("print x >> '{}'\n", tally.display());
        std::fs::write(&boot, line + body).unwrap();
    };
    let captures = || {
        std::fs::read_to_string(&tally)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    };

    write_boot("export MEMO_PROBE=1\n");
    assert_eq!(sharezed(&state, &boot, &[]).0, 0, "first publish");

    write_boot("export MEMO_PROBE=2\n");
    assert_eq!(sharezed(&state, &boot, &["--if-noop"]).0, 1, "delta");
    let n = captures();
    assert_eq!(sharezed(&state, &boot, &["--if-noop"]).0, 1, "same delta");
    assert_eq!(
        captures(),
        n,
        "nothing moved: the answer must come from memory"
    );

    // Moving a tracked file invalidates it — even back to something harmless.
    write_boot("export MEMO_PROBE=2 \n");
    assert_eq!(
        sharezed(&state, &boot, &["--if-noop"]).0,
        1,
        "still a delta vs the published state"
    );
    assert!(captures() > n, "a changed file must be re-captured");

    let _ = std::fs::remove_dir_all(&tmp);
}
