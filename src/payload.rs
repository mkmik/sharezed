//! Apply-payload generation (PRD Appendix B).
//!
//! Every assignment is guarded by the value sharezed itself last published for
//! that key, so a locally edited key is skipped rather than clobbered (§7.4,
//! ours-wins). The guard means the shell never has to ship its whole state to
//! the tool — only the ordered-list params, which need element-wise merging.

use crate::merge::merge;
use crate::state::{Change, Item, Kind, State, is_list};
use glob::Pattern;

/// Single-quote for zsh.
pub fn zq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && !n.starts_with(|c: char| c.is_ascii_digit())
        && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn flags(kind: Kind, attrs: &str) -> String {
    let mut f = String::from("-g");
    match kind {
        Kind::Array => f.push('a'),
        Kind::Assoc => f.push('A'),
        _ => {}
    }
    for (attr, c) in [
        ("export", 'x'),
        ("unique", 'U'),
        ("integer", 'i'),
        ("float", 'F'),
    ] {
        if attrs.contains(attr) {
            f.push(c);
        }
    }
    f
}

/// Aliases and functions go through the builtins, not through `functions[x]=`:
/// a subscript in an assignment is taken *literally*, so `functions['gs']=…`
/// defines a function whose name includes the quotes. Verified on zsh 5.9.
fn set_stmt(c: &Change, vals: &[String]) -> Option<String> {
    let one = || vals.first().cloned().unwrap_or_default();
    Some(match c.kind {
        // Recorded by presence, so the body is irrelevant — and `fpath` is an
        // ordered-list param, applied earlier in the same entry (§8.7).
        Kind::Autoload => format!("autoload -Uz -- {}", zq(&c.name)),
        // The body came out of $functions, so it is always re-parsable source.
        Kind::Func => format!("function {} {{\n{}\n}}", zq(&c.name), one()),
        Kind::Alias => format!("alias -- {}", zq(&format!("{}={}", c.name, one()))),
        Kind::Galias => format!("alias -g -- {}", zq(&format!("{}={}", c.name, one()))),
        Kind::Salias => format!("alias -s -- {}", zq(&format!("{}={}", c.name, one()))),
        // A shell that never ran `compinit` has no `compdef` and nothing to
        // bind — it must skip this quietly, not fail on every prompt. The
        // command name is quoted because `-default-` and friends are real keys.
        Kind::Compdef => format!(
            "(( $+functions[compdef] )) && compdef -- {} {}",
            zq(&one()),
            zq(&c.name)
        ),
        _ if !valid_name(&c.name) => return None,
        Kind::Scalar => format!(
            "typeset {} {}={}",
            flags(c.kind, &c.attrs),
            c.name,
            zq(&one())
        ),
        _ => {
            let q: Vec<String> = vals.iter().map(|v| zq(v)).collect();
            format!(
                "typeset {} {}=( {} )",
                flags(c.kind, &c.attrs),
                c.name,
                q.join(" ")
            )
        }
    })
}

fn unset_stmt(c: &Change) -> Option<String> {
    Some(match c.kind {
        Kind::Func | Kind::Autoload => format!("unfunction -- {}", zq(&c.name)),
        // `unalias -g` is not accepted; plain unalias removes a global alias.
        Kind::Alias | Kind::Galias => format!("unalias -- {}", zq(&c.name)),
        Kind::Salias => format!("unalias -s -- {}", zq(&c.name)),
        Kind::Compdef => format!(
            "(( $+functions[compdef] )) && compdef -d -- {}",
            zq(&c.name)
        ),
        _ if valid_name(&c.name) => format!("unset {}", c.name),
        _ => return None,
    })
}

/// A guarded statement: apply only if this key is still what sharezed put
/// there, otherwise record a conflict and leave the local edit alone (§7.4).
fn guarded(c: &Change, stmt: &str) -> String {
    let (kind, name) = (c.kind.as_str(), zq(&c.name));
    let eq = |vals: &[String]| {
        let args: String = vals.iter().map(|v| format!(" {}", zq(v))).collect();
        format!("_sharezed_eq {kind} {name}{args}")
    };
    let absent = format!("_sharezed_absent {kind} {name}");
    match (&c.old, &c.new) {
        // Tombstone: already gone is success, not a conflict.
        (Some(old), None) => format!(
            "if {}; then\n  {stmt}\nelif {absent}; then\n  :\nelse\n  _sharezed_conflict {name}\nfi\n",
            eq(old)
        ),
        // New key: a shell that ran the same bootstrap already has the right
        // value — converging on it is not a conflict either.
        (None, Some(new)) => format!(
            "if {absent} || {}; then\n  {stmt}\nelse\n  _sharezed_conflict {name}\nfi\n",
            eq(new)
        ),
        // Already holding the new value is not a conflict — applying it is a
        // no-op. Without this, a shell that started after the publish reports
        // one for every key in the entry.
        (Some(old), Some(new)) => format!(
            "if {} || {}; then\n  {stmt}\nelse\n  _sharezed_conflict {name}\nfi\n",
            eq(old),
            eq(new)
        ),
        (None, None) => String::new(),
    }
}

/// Ordered-list params first (`fpath` must land before any autoload stub in the
/// same entry, §8.7), then the rest of the params, then functions, then aliases
/// and `compdef` bindings — which have to follow the function they name.
fn order(c: &Change) -> u8 {
    match c.kind {
        // The apply machinery rewriting itself, last: the guards for every
        // other key in the entry then run on one consistent version of it.
        _ if c.name.starts_with("_sharezed_") => 4,
        _ if is_list(c.kind, &c.attrs) => 0,
        Kind::Scalar | Kind::Array | Kind::Assoc => 1,
        Kind::Func | Kind::Autoload => 2,
        _ => 3,
    }
}

/// `ours` carries the live value of the ordered-list params; it is updated as
/// entries are applied so a multi-generation catch-up merges against the right
/// intermediate state.
pub fn generate(entries: &[(u64, Vec<Change>)], ours: &mut State, families: &[Pattern]) -> String {
    let mut code = String::new();
    for (seq, changes) in entries {
        let mut changes: Vec<&Change> = changes.iter().collect();
        changes.sort_by_key(|c| order(c));
        code.push_str(&format!("# entry {seq}\n"));
        for c in changes {
            match (&c.new, is_list(c.kind, &c.attrs)) {
                (Some(theirs), true) => {
                    let key = c.key();
                    // No `old` means this key is new to the log, so the merge
                    // base is whatever the shell had when the hook was
                    // installed — §8.9, without which every element of the
                    // live list reads as a local prepend.
                    let base0 = ours.get(&(c.kind, format!("@base:{}", c.name)));
                    let base = c
                        .old
                        .as_deref()
                        .or(base0.map(|i| &i.vals[..]))
                        .unwrap_or_default();
                    let list = match ours.get(&key) {
                        Some(live) => {
                            let m = merge(base, &live.vals, theirs, families);
                            for n in &m.notes {
                                code.push_str(&format!("# {}: {n}\n", c.name));
                            }
                            m.list
                        }
                        // Not present in this shell at all: nothing local to preserve.
                        None => theirs.clone(),
                    };
                    let vals: Vec<String> = list.iter().map(|v| zq(v)).collect();
                    code.push_str(&format!(
                        "typeset {} {}=( {} )\n",
                        flags(c.kind, &c.attrs),
                        c.name,
                        vals.join(" ")
                    ));
                    ours.insert(
                        key,
                        Item {
                            attrs: c.attrs.clone(),
                            vals: list,
                        },
                    );
                }
                (new, _) => {
                    let stmt = match new {
                        Some(vals) => set_stmt(c, vals),
                        None => unset_stmt(c),
                    };
                    let Some(stmt) = stmt else { continue };
                    // theirs-wins for sharezed's own functions (§7.4 allows a
                    // per-key policy). Guarding them means a shell holding the
                    // hook its own zshrc installed reads as a local edit, so it
                    // would keep an old hook forever. Nobody hand-edits these.
                    if c.name.starts_with("_sharezed_") {
                        code.push_str(&stmt);
                        code.push('\n');
                    } else {
                        code.push_str(&guarded(c, &stmt));
                    }
                }
            }
        }
        code.push_str(&format!("SHAREZED_CURSOR={seq}\n"));
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Item;

    fn change(
        kind: Kind,
        name: &str,
        attrs: &str,
        new: Option<&[&str]>,
        old: Option<&[&str]>,
    ) -> Change {
        let own = |v: Option<&[&str]>| v.map(|v| v.iter().map(|s| s.to_string()).collect());
        Change {
            kind,
            name: name.into(),
            attrs: attrs.into(),
            new: own(new),
            old: own(old),
        }
    }

    #[test]
    fn quoting_survives_hostile_values() {
        assert_eq!(zq("it's"), r"'it'\''s'");
        let c = change(
            Kind::Scalar,
            "X",
            "scalar-export",
            Some(&["a'b; rm -rf /"]),
            None,
        );
        let p = generate(&[(1, vec![c])], &mut State::new(), &[]);
        assert!(p.contains(r"typeset -gx X='a'\''b; rm -rf /'"), "{}", p);
        assert!(p.contains("_sharezed_absent scalar 'X' || _sharezed_eq scalar 'X'"));
        assert!(p.ends_with("SHAREZED_CURSOR=1\n"));
    }

    #[test]
    fn edits_are_guarded_by_the_previously_published_value() {
        let c = change(
            Kind::Func,
            "work",
            "",
            Some(&["print new"]),
            Some(&["print old"]),
        );
        let p = generate(&[(7, vec![c])], &mut State::new(), &[]);
        assert!(p.contains("_sharezed_eq func 'work' 'print old'"), "{}", p);
        assert!(p.contains("function 'work' {\nprint new\n}"), "{}", p);
        assert!(p.contains("_sharezed_conflict 'work'"));
    }

    #[test]
    fn tombstones_and_autoload_stubs() {
        let tomb = change(Kind::Alias, "gs", "", None, Some(&["git status"]));
        let stub = change(Kind::Autoload, "zmv", "", Some(&[]), None);
        let add = change(Kind::Alias, "ll", "", Some(&["ls -l"]), None);
        let p = generate(&[(2, vec![tomb, stub, add])], &mut State::new(), &[]);
        assert!(p.contains("unalias -- 'gs'"), "{}", p);
        // a tombstone for a key this shell never had is a no-op, not a conflict
        assert!(
            p.contains("elif _sharezed_absent alias 'gs'; then\n  :"),
            "{}",
            p
        );
        assert!(p.contains("autoload -Uz -- 'zmv'"), "{}", p);
        assert!(p.contains("alias -- 'll=ls -l'"), "{}", p);
    }

    /// A completion is two things — the function and the `compdef` that binds
    /// it to a command — and the binding is useless before the function exists.
    #[test]
    fn a_compdef_binding_lands_after_the_function_it_names() {
        let f = change(Kind::Func, "_ccwt", "", Some(&["print hi"]), None);
        let b = change(Kind::Compdef, "ccwt", "", Some(&["_ccwt"]), None);
        // reversed on purpose: `order` is what puts them right, not the input
        let p = generate(&[(1, vec![b, f])], &mut State::new(), &[]);
        let bind = "(( $+functions[compdef] )) && compdef -- '_ccwt' 'ccwt'";
        assert!(p.contains(bind), "{p}");
        assert!(
            p.find("function '_ccwt'").unwrap() < p.find(bind).unwrap(),
            "{p}"
        );
        assert!(p.contains("_sharezed_absent compdef 'ccwt'"), "{p}");

        let gone = change(Kind::Compdef, "ccwt", "", None, Some(&["_ccwt"]));
        let p = generate(&[(2, vec![gone])], &mut State::new(), &[]);
        assert!(
            p.contains("(( $+functions[compdef] )) && compdef -d -- 'ccwt'"),
            "{p}"
        );
    }

    #[test]
    fn a_new_list_param_merges_against_the_hook_install_base() {
        let c = change(
            Kind::Array,
            "path",
            "array-tied",
            Some(&["/opt/x", "/usr/bin"]),
            None,
        );
        let mut ours = State::new();
        let item = |v: &[&str]| Item {
            attrs: "array-tied".into(),
            vals: v.iter().map(|s| s.to_string()).collect(),
        };
        ours.insert((Kind::Array, "path".into()), item(&["/me/bin", "/usr/bin"]));
        ours.insert((Kind::Array, "@base:path".into()), item(&["/usr/bin"]));
        let p = generate(&[(1, vec![c])], &mut ours, &[]);
        // /me/bin is the only local addition; /opt/x keeps its published slot.
        assert!(
            p.contains("path=( '/me/bin' '/opt/x' '/usr/bin' )"),
            "{}",
            p
        );
    }

    #[test]
    fn list_params_merge_instead_of_overwriting() {
        let c = change(
            Kind::Array,
            "path",
            "array-tied-unique-special",
            Some(&["/usr/bin", "/opt/new"]),
            Some(&["/usr/bin"]),
        );
        let mut ours = State::new();
        ours.insert(
            (Kind::Array, "path".into()),
            Item {
                attrs: "array-tied-unique-special".into(),
                vals: vec!["/home/m/bin".into(), "/usr/bin".into()],
            },
        );
        let p = generate(&[(3, vec![c])], &mut ours, &[]);
        assert!(
            p.contains("typeset -gaU path=( '/home/m/bin' '/usr/bin' '/opt/new' )"),
            "{}",
            p
        );
        // ours is advanced so the next entry merges against the right state
        assert_eq!(ours[&(Kind::Array, "path".into())].vals.len(), 3);
    }
}
