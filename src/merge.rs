//! Ordered-list merge: anchored rebase for `PATH` and friends (PRD §8).
//!
//! A local addition is recorded with its nearest surviving predecessor, then
//! rebased onto the incoming list. "Predecessor = HEAD" *is* what a prepend is,
//! so prepend-ness is derived rather than special-cased.

use glob::Pattern;
use std::collections::HashSet;

/// Lexical normalization only — never `realpath`: symlink resolution changes
/// meaning and costs syscalls (§8.1).
pub fn norm(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_slash = false;
    for c in s.chars() {
        if c == '/' && prev_slash {
            continue;
        }
        prev_slash = c == '/';
        out.push(c);
    }
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// Keep first, matching zsh's `typeset -U` (verified: `-U` keeps the first
/// occurrence, which is also PATH's first-match-wins semantics).
fn dedup(v: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    v.iter().filter(|e| seen.insert(norm(e))).cloned().collect()
}

/// Indices into `ours` that survive from `base` in the same relative order.
fn lcs_anchors(base: &[String], ours: &[String]) -> HashSet<usize> {
    let (nb, no) = (base.len(), ours.len());
    let (b, o): (Vec<_>, Vec<_>) = (
        base.iter().map(|s| norm(s)).collect(),
        ours.iter().map(|s| norm(s)).collect(),
    );
    // ponytail: O(n²) LCS. n is 20–60 for a PATH, so ~3600 ops — free.
    let mut dp = vec![vec![0usize; no + 1]; nb + 1];
    for i in (0..nb).rev() {
        for j in (0..no).rev() {
            dp[i][j] = if b[i] == o[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let (mut i, mut j, mut anchors) = (0, 0, HashSet::new());
    while i < nb && j < no {
        if b[i] == o[j] {
            anchors.insert(j);
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    anchors
}

pub struct Merged {
    pub list: Vec<String>,
    /// Every non-trivial resolution, so a surprising PATH is traceable to a
    /// rule rather than to vibes (`sharezed path explain`).
    pub notes: Vec<String>,
}

/// `base` — what sharezed last installed here. `ours` — the live list.
/// `theirs` — the incoming list. Caller then sets base := theirs (§8.3).
pub fn merge(base: &[String], ours: &[String], theirs: &[String], families: &[Pattern]) -> Merged {
    let (base, ours, theirs) = (dedup(base), dedup(ours), dedup(theirs));
    let anchors = lcs_anchors(&base, &ours);
    let tset: HashSet<String> = theirs.iter().map(|e| norm(e)).collect();
    let mut notes = Vec::new();

    // Local additions, each tagged with its nearest surviving predecessor
    // (None == HEAD).
    let mut local: Vec<(String, Option<String>)> = Vec::new();
    for (i, e) in ours.iter().enumerate() {
        if anchors.contains(&i) {
            continue;
        }
        let pred = (0..i)
            .rev()
            .find(|j| anchors.contains(j))
            .map(|j| norm(&ours[j]));
        local.push((e.clone(), pred));
    }

    // The user's removal of a managed entry is a local edit; ours-wins (§7.4).
    let ourset: HashSet<String> = ours.iter().map(|e| norm(e)).collect();
    let removed: HashSet<String> = base
        .iter()
        .map(|e| norm(e))
        .filter(|e| !ourset.contains(e))
        .collect();
    let mut result: Vec<String> = theirs
        .iter()
        .filter(|e| {
            let gone = removed.contains(&norm(e));
            if gone {
                notes.push(format!("kept deleted: {e} (upstream still wants it)"));
            }
            !gone
        })
        .cloned()
        .collect();

    // Family eviction — a newer member from upstream retires an older local
    // prepend, so `nvm use 18` doesn't shadow upstream's v20 forever (§8.4).
    for pat in families {
        if result.iter().any(|x| pat.matches(&norm(x))) {
            local.retain(|(e, _)| {
                let evicted = pat.matches(&norm(e));
                if evicted {
                    notes.push(format!("evicted: {e} (superseded in family {pat})"));
                }
                !evicted
            });
        }
    }

    // Reverse ⇒ multiple head-prepends keep their relative order.
    for (e, pred) in local.iter().rev() {
        let ne = norm(e);
        if tset.contains(&ne) {
            if pred.is_none() {
                // You prepended it: that was a priority assertion, honor it.
                result.retain(|x| norm(x) != ne);
                result.insert(0, e.clone());
                notes.push(format!("hoisted: {e}"));
            }
            continue;
        }
        match pred {
            None => result.insert(0, e.clone()),
            Some(p) => match result.iter().position(|x| &norm(x) == p) {
                Some(idx) => result.insert(idx + 1, e.clone()),
                None => {
                    result.insert(0, e.clone());
                    notes.push(format!("orphan: {e} (anchor {p} gone)"));
                }
            },
        }
    }
    Merged {
        list: dedup(&result),
        notes,
    }
}

/// Toolchain switchers whose entries supersede each other (§8.4). Extend with
/// `SHAREZED_PATH_FAMILIES` (space-separated globs).
pub fn default_families() -> Vec<Pattern> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut pats: Vec<String> = [
        "~/.nvm/versions/node/*/bin",
        "~/.pyenv/versions/*/bin",
        "~/.rbenv/versions/*/bin",
        "~/.rustup/toolchains/*/bin",
        "~/.asdf/installs/*/*/bin",
    ]
    .iter()
    .map(|p| p.replace('~', &home))
    .collect();
    pats.extend(
        std::env::var("SHAREZED_PATH_FAMILIES")
            .unwrap_or_default()
            .split_whitespace()
            .map(|p| p.replace('~', &home)),
    );
    pats.iter().filter_map(|p| Pattern::new(p).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }
    fn m(b: &[&str], o: &[&str], t: &[&str]) -> Merged {
        merge(&v(b), &v(o), &v(t), &[])
    }

    // The §8.5 table: 12 scenarios, all of which must hold.

    #[test]
    fn s1_prepends_survive_an_upstream_insert() {
        let r = m(
            &["/usr/bin", "/bin"],
            &["/home/m/bin", "/usr/bin", "/bin"],
            &["/usr/bin", "/opt/new", "/bin"],
        );
        assert_eq!(r.list, v(&["/home/m/bin", "/usr/bin", "/opt/new", "/bin"]));
    }

    #[test]
    fn s2_three_prepends_keep_their_relative_order() {
        let r = m(
            &["/usr/bin"],
            &["/a", "/b", "/c", "/usr/bin"],
            &["/usr/bin", "/bin"],
        );
        assert_eq!(r.list, v(&["/a", "/b", "/c", "/usr/bin", "/bin"]));
    }

    #[test]
    fn s3_reapplying_a_generation_is_idempotent() {
        let (base, theirs) = (v(&["/usr/bin", "/bin"]), v(&["/usr/bin", "/bin"]));
        let once = merge(
            &base,
            &v(&["/home/m/bin", "/usr/bin", "/bin"]),
            &theirs,
            &[],
        );
        // base := theirs, not the merged result (§8.3) — that is what makes it idempotent.
        let twice = merge(&base, &once.list, &theirs, &[]);
        assert_eq!(once.list, twice.list);
        assert_eq!(once.list, v(&["/home/m/bin", "/usr/bin", "/bin"]));
    }

    #[test]
    fn s4_a_local_append_is_not_promoted() {
        let r = m(
            &["/usr/bin", "/bin"],
            &["/usr/bin", "/bin", "/tail"],
            &["/usr/bin", "/bin"],
        );
        assert_eq!(r.list, v(&["/usr/bin", "/bin", "/tail"]));
    }

    #[test]
    fn s5_a_mid_list_insert_follows_its_anchor_through_a_reversal() {
        let r = m(
            &["/a", "/b", "/c"],
            &["/a", "/b", "/mine", "/c"],
            &["/c", "/b", "/a"],
        );
        assert_eq!(r.list, v(&["/c", "/b", "/mine", "/a"]));
    }

    #[test]
    fn s6_a_locally_deleted_entry_stays_deleted() {
        let r = m(&["/a", "/b"], &["/a"], &["/a", "/b"]);
        assert_eq!(r.list, v(&["/a"]));
        assert!(r.notes.iter().any(|n| n.starts_with("kept deleted: /b")));
    }

    #[test]
    fn s7_prepending_something_upstream_has_lower_down_hoists_it() {
        let r = m(
            &["/a", "/b", "/c"],
            &["/c", "/a", "/b"],
            &["/a", "/b", "/c"],
        );
        assert_eq!(r.list, v(&["/c", "/a", "/b"]));
        assert!(r.notes.iter().any(|n| n.starts_with("hoisted: /c")));
    }

    #[test]
    fn s8_upstream_drops_an_untouched_entry() {
        let r = m(&["/a", "/b"], &["/a", "/b"], &["/a"]);
        assert_eq!(r.list, v(&["/a"]));
    }

    #[test]
    fn s9_an_insert_whose_anchor_vanished_is_flagged_orphan() {
        let r = m(&["/a", "/b"], &["/a", "/mine", "/b"], &["/b"]);
        assert_eq!(r.list, v(&["/mine", "/b"]));
        assert!(r.notes.iter().any(|n| n.contains("orphan: /mine")));
    }

    #[test]
    fn s10_trailing_slashes_are_the_same_entry() {
        let r = m(
            &["/usr/bin"],
            &["/opt/x/", "/usr/bin"],
            &["/opt/x//", "/usr/bin"],
        );
        assert_eq!(r.list.len(), 2, "no duplicate: {:?}", r.list);
        assert_eq!(norm("/opt/x/"), norm("/opt/x//"));
    }

    #[test]
    fn s11_a_newer_family_member_evicts_the_local_prepend() {
        let fam = [Pattern::new("/nvm/*/bin").unwrap()];
        let r = merge(
            &v(&["/usr/bin"]),
            &v(&["/nvm/v18/bin", "/usr/bin"]),
            &v(&["/nvm/v20/bin", "/usr/bin"]),
            &fam,
        );
        assert_eq!(r.list, v(&["/nvm/v20/bin", "/usr/bin"]));
        assert!(r.notes.iter().any(|n| n.contains("evicted: /nvm/v18/bin")));
    }

    #[test]
    fn s12_an_empty_component_is_preserved() {
        let r = m(&["/usr/bin"], &["", "/usr/bin"], &["/usr/bin", "/bin"]);
        assert_eq!(r.list, v(&["", "/usr/bin", "/bin"]));
    }

    #[test]
    fn norm_is_lexical_only() {
        assert_eq!(norm("//a//b//"), "/a/b");
        assert_eq!(norm("/"), "/");
        assert_eq!(norm(""), "");
        assert_eq!(dedup(&v(&["/a", "/a/", "/b", "/a//"])), v(&["/a", "/b"]));
    }
}
