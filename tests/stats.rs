//! `sharezed stats` exists to answer "was that stall at my prompt sharezed?",
//! so what has to hold is that a real precmd leaves a line behind with the
//! forks it made, and that `stats` reads it back as a row per step.

use std::process::Command;

#[test]
fn every_prompt_leaves_a_timing_and_stats_reads_it() {
    if Command::new("zsh").arg("-fc").arg("true").status().is_err() {
        return;
    }
    let state = std::env::temp_dir().join(format!("sharezed-stats-{}", std::process::id()));
    std::fs::create_dir_all(&state).unwrap();
    let bin = env!("CARGO_BIN_EXE_sharezed");
    let run = |args: &[&str]| {
        Command::new(bin)
            .args(args)
            .env("XDG_STATE_HOME", &state)
            .output()
            .unwrap()
    };
    let hook = String::from_utf8_lossy(&run(&["hook", "zsh"]).stdout).into_owned();

    let out = Command::new("zsh")
        .args(["-f", "-c"])
        .arg(format!("{hook}\n_sharezed_precmd\n_sharezed_precmd"))
        .env("XDG_STATE_HOME", &state)
        .env_remove("SHAREZED_NO_NOTIFY")
        .env_remove("SHAREZED_DISABLE")
        .output()
        .unwrap();
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let timings = std::fs::read_to_string(state.join("sharezed/user/timings")).unwrap();
    let lines: Vec<&str> = timings.lines().collect();
    assert_eq!(lines.len(), 2, "one line per prompt: {timings}");
    for l in &lines {
        let w: Vec<&str> = l.split(' ').collect();
        assert!(w[0].parse::<u64>().unwrap() > 1_000_000_000, "epoch: {l}");
        let total: u64 = w[1].parse().unwrap();
        let check: u64 = w[2].strip_prefix("check:").unwrap().parse().unwrap();
        assert!(
            check > 0 && check <= total,
            "a step fits in its prompt: {l}"
        );
    }

    let stats = String::from_utf8_lossy(&run(&["stats"]).stdout).into_owned();
    assert!(stats.starts_with("2 prompts"), "{stats}");
    let rows: Vec<&str> = stats.lines().skip(2).collect();
    assert!(
        rows[0].starts_with("precmd") && rows[1].starts_with("check"),
        "{stats}"
    );
    assert_eq!(rows.len(), 2, "no row for a step that never ran: {stats}");

    let _ = std::fs::remove_dir_all(&state);
}
