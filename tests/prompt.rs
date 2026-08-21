//! Prompts are `scalar-special`, which the capture used to drop wholesale.
//! What has to hold: an edit to PS1 publishes, an edit to something the shell
//! derives from its terminal (TERM, COLUMNS) does not, the diff you read
//! before trusting the entry doesn't repaint your terminal on the way past,
//! and RPROMPT survives the nag the hook keeps parking in it.

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

#[test]
fn prompts_sync_and_terminal_state_does_not() {
    if !has_zsh() {
        return;
    }
    let tmp = std::env::temp_dir().join(format!("sharezed-prompt-{}", std::process::id()));
    let (state, boot) = (tmp.join("state"), tmp.join("boot.zsh"));
    std::fs::create_dir_all(&state).unwrap();
    // PROMPT, not PS1: the same parameter under its other name, which is what
    // proves capturing one spelling is enough.
    std::fs::write(&boot, "PROMPT='old%# '\nRPROMPT='%*'\nTERM=xterm\n").unwrap();
    assert_eq!(sharezed(&state, &boot, &[]).0, 0, "first publish");

    std::fs::write(&boot, "PROMPT=$'\\e[7mnew%# '\nRPROMPT='%~'\nTERM=dumb\n").unwrap();
    let (code, out) = sharezed(&state, &boot, &["--dry-run"]);
    assert_eq!(code, 1, "a prompt edit is a delta: {out}");
    assert!(out.contains("PS1"), "captured under its PS* name: {out}");
    assert!(out.contains("old%#") && out.contains("new%#"), "{out}");
    assert!(out.contains("RPROMPT"), "{out}");
    assert!(!out.contains("PROMPT2"), "one record per prompt: {out}");
    assert!(
        !out.contains("TERM") && !out.contains("xterm"),
        "TERM is this terminal's, never the log's: {out}"
    );
    assert!(
        !out.contains('\x1b'),
        "an escape in a value must not reach the terminal raw: {out:?}"
    );
    assert!(out.contains("\\e[7m"), "shown as \\e: {out}");

    let _ = std::fs::remove_dir_all(&tmp);
}

/// The nag lives in RPROMPT and RPROMPT is synced state now, so an apply that
/// carries a new right prompt has to run against the stripped value. Get the
/// order wrong and every shell reads its own nag as a local edit and keeps the
/// old prompt forever — quietly, since a conflict only shows in `status`.
#[test]
fn an_apply_lands_through_the_nag_the_last_prompt_left() {
    if !has_zsh() {
        return;
    }
    let tmp = std::env::temp_dir().join(format!("sharezed-nag-{}", std::process::id()));
    let (state, boot) = (tmp.join("state"), tmp.join("boot.zsh"));
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(&boot, "PS1='A '\nRPROMPT='R1'\n").unwrap();
    assert_eq!(sharezed(&state, &boot, &[]).0, 0, "gen 1");

    let hook = Command::new(env!("CARGO_BIN_EXE_sharezed"))
        .args(["hook", "zsh"])
        .env("XDG_STATE_HOME", &state)
        .output()
        .expect("hook zsh");
    let hook = String::from_utf8_lossy(&hook.stdout).into_owned();

    std::fs::write(&boot, "PS1='B '\nRPROMPT='R2'\n").unwrap();
    assert_eq!(sharezed(&state, &boot, &[]).0, 0, "gen 2");

    // A shell still at gen 1, wearing the nag its last prompt put there.
    let out = Command::new("zsh")
        .args(["-f", "-c"])
        .arg(format!(
            "{hook}\nSHAREZED_CURSOR=1\nPS1='A '\nRPROMPT=\"R1$_sharezed_segment\"\n\
             _sharezed_precmd\nprint -r -- \"$PS1|$RPROMPT|$SHAREZED_CONFLICTS\""
        ))
        .env("XDG_STATE_HOME", &state)
        .env("SHAREZED_BOOTSTRAP", &boot)
        .env_remove("SHAREZED_NO_NOTIFY")
        .env_remove("SHAREZED_DISABLE")
        .output()
        .unwrap();
    // Nothing has moved since gen 2 published, so the nag does not come back.
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "B |R2|",
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
