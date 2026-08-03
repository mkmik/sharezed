//! `reload --dry-run` is only useful if its exit code is trustworthy and it
//! publishes nothing. Both are end-to-end properties, so this drives the binary.

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

#[test]
fn dry_run_reports_without_publishing() {
    if Command::new("zsh").arg("-fc").arg("true").status().is_err() {
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
