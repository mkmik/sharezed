//! Shell state: the wire format, the typed state map, and the diff (PRD §5, §7.2).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Scalar,
    Array,
    Assoc,
    Func,
    Autoload,
    Alias,
    Galias,
    Salias,
    /// A `compdef` binding: `name` is the command, the value the completion
    /// function. The rest of `_comps` is compinit's own and never syncs.
    Compdef,
}

impl Kind {
    /// Namespace the consumer-side helpers look in.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Scalar => "scalar",
            Kind::Array => "array",
            Kind::Assoc => "assoc",
            Kind::Func => "func",
            Kind::Autoload => "autoload",
            Kind::Alias => "alias",
            Kind::Galias => "galias",
            Kind::Salias => "salias",
            Kind::Compdef => "compdef",
        }
    }
}

pub type Key = (Kind, String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub attrs: String,
    pub vals: Vec<String>,
}

pub type State = BTreeMap<Key, Item>;

/// One key's transition between two generations. `new: None` is a tombstone;
/// `old` is the merge base — what sharezed itself last published for this key.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Change {
    pub kind: Kind,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub attrs: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<Vec<String>>,
}

impl Change {
    pub fn key(&self) -> Key {
        (self.kind, self.name.clone())
    }
}

/// An ordered-list param (`path`, `fpath`, …) — merged element-wise per §8
/// instead of going through the whole-value three-way merge.
///
/// ponytail: tied arrays only. `list_params_scalar` (§8.7, `LD_LIBRARY_PATH`)
/// needs a config file; add it when someone actually syncs one.
pub fn is_list(kind: Kind, attrs: &str) -> bool {
    kind == Kind::Array && attrs.contains("tied")
}

/// Parse the NUL-delimited capture stream: `kind \0 name \0 meta \0 nvals \0 val₁ … \0`.
pub fn parse_wire(buf: &[u8]) -> Result<State, String> {
    // ponytail: lossy UTF-8. Shell params are bytes; JSON is not. Non-UTF-8
    // values would need a bytes-aware log format.
    let mut f = buf.split(|&b| b == 0).map(String::from_utf8_lossy);
    let mut state = State::new();
    while let Some(kind) = f.next() {
        if kind.is_empty() {
            break; // trailing NUL
        }
        let (name, attrs, n) = match (f.next(), f.next(), f.next()) {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => return Err(format!("truncated record for {kind}")),
        };
        let n: usize = n
            .parse()
            .map_err(|_| format!("bad nvals {n:?} for {name}"))?;
        let mut vals = Vec::with_capacity(n);
        for _ in 0..n {
            vals.push(
                f.next()
                    .ok_or_else(|| format!("truncated values for {name}"))?
                    .into_owned(),
            );
        }
        let kind = match (&*kind, &*attrs) {
            ("param", a) if a.starts_with("association") => Kind::Assoc,
            ("param", a) if a.starts_with("array") => Kind::Array,
            ("param", _) => Kind::Scalar,
            ("func", _) => Kind::Func,
            ("autoload", _) => Kind::Autoload,
            ("alias", _) => Kind::Alias,
            ("galias", _) => Kind::Galias,
            ("salias", _) => Kind::Salias,
            ("compdef", _) => Kind::Compdef,
            (k, _) => return Err(format!("unknown kind {k:?}")),
        };
        state.insert(
            (kind, name.into_owned()),
            Item {
                attrs: attrs.into_owned(),
                vals,
            },
        );
    }
    Ok(state)
}

/// The desired state: what the bootstrap script *adds or changes* relative to a
/// clean shell. Diffing successive desired states is what makes deletions
/// first-class (§2).
///
/// ponytail: keys the bootstrap *unsets* (in S₀, gone from S₁) are dropped, not
/// carried as tombstones. Add if `unset` in a zshrc ever needs to propagate.
pub fn effect(s0: &State, s1: &State, ignore: &[glob::Pattern]) -> State {
    s1.iter()
        .filter(|(k, v)| s0.get(*k) != Some(v))
        .filter(|(k, _)| !ignore.iter().any(|p| p.matches(&k.1)))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Drop session-scoped elements from ordered lists. A PATH entry under
/// `$TMPDIR` is per-session by construction — cmux mints
/// `$TMPDIR/cmux-cli-shims/$CMUX_PANEL_ID` per terminal panel — so publishing
/// it churns a generation on every reload from a different terminal and leaks
/// one shell's temp dir into all the others. cmux's own shell integration
/// strips these by the same kind of glob.
pub fn drop_volatile(state: &mut State, globs: &[glob::Pattern]) {
    for ((kind, _), item) in state.iter_mut() {
        if is_list(*kind, &item.attrs) {
            item.vals.retain(|e| {
                let n = crate::merge::norm(e);
                !globs.iter().any(|g| g.matches(&n))
            });
        }
    }
}

/// Δₙ = diff(Sₙ₋₁, Sₙ).
pub fn diff(prev: &State, next: &State) -> Vec<Change> {
    let mut out = Vec::new();
    for ((kind, name), item) in next {
        let old = prev.get(&(*kind, name.clone()));
        if old.map(|o| &o.vals) == Some(&item.vals) && old.map(|o| &o.attrs) == Some(&item.attrs) {
            continue;
        }
        out.push(Change {
            kind: *kind,
            name: name.clone(),
            attrs: item.attrs.clone(),
            new: Some(item.vals.clone()),
            old: old.map(|o| o.vals.clone()),
        });
    }
    for ((kind, name), item) in prev {
        if !next.contains_key(&(*kind, name.clone())) {
            out.push(Change {
                kind: *kind,
                name: name.clone(),
                attrs: item.attrs.clone(),
                new: None,
                old: Some(item.vals.clone()),
            });
        }
    }
    out
}

pub fn apply(state: &mut State, changes: &[Change]) {
    for c in changes {
        match &c.new {
            Some(vals) => {
                state.insert(
                    c.key(),
                    Item {
                        attrs: c.attrs.clone(),
                        vals: vals.clone(),
                    },
                );
            }
            None => {
                state.remove(&c.key());
            }
        }
    }
}

/// `+2 functions, ~1 param, -1 alias`
pub fn summary(changes: &[Change]) -> String {
    let mut parts = Vec::new();
    for (one, many, kinds) in [
        (
            "param",
            "params",
            &[Kind::Scalar, Kind::Array, Kind::Assoc][..],
        ),
        ("function", "functions", &[Kind::Func, Kind::Autoload][..]),
        (
            "alias",
            "aliases",
            &[Kind::Alias, Kind::Galias, Kind::Salias][..],
        ),
        ("compdef", "compdefs", &[Kind::Compdef][..]),
    ] {
        let of = |f: &dyn Fn(&Change) -> bool| {
            changes
                .iter()
                .filter(|c| kinds.contains(&c.kind) && f(c))
                .count()
        };
        for (sign, n) in [
            ("+", of(&|c| c.new.is_some() && c.old.is_none())),
            ("~", of(&|c| c.new.is_some() && c.old.is_some())),
            ("-", of(&|c| c.new.is_none())),
        ] {
            if n > 0 {
                parts.push(format!("{sign}{n} {}", if n == 1 { one } else { many }));
            }
        }
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(recs: &[&[&str]]) -> Vec<u8> {
        let mut out = Vec::new();
        for r in recs {
            for f in *r {
                out.extend_from_slice(f.as_bytes());
                out.push(0);
            }
        }
        out
    }

    #[test]
    fn parses_every_kind() {
        let s = parse_wire(&wire(&[
            &["param", "EDITOR", "scalar-export", "1", "vim"],
            &[
                "param",
                "path",
                "array-tied-special",
                "2",
                "/bin",
                "/usr/bin",
            ],
            &["param", "A", "association", "2", "k", "v"],
            &["param", "EMPTY", "array", "0"],
            &["func", "work", "", "1", "print hi"],
            &["autoload", "zmv", "", "0"],
            &["alias", "gs", "", "1", "git status"],
        ]))
        .unwrap();
        assert_eq!(s[&(Kind::Scalar, "EDITOR".into())].vals, ["vim"]);
        assert_eq!(s[&(Kind::Array, "path".into())].vals, ["/bin", "/usr/bin"]);
        assert_eq!(s[&(Kind::Assoc, "A".into())].vals, ["k", "v"]);
        assert!(s[&(Kind::Array, "EMPTY".into())].vals.is_empty());
        assert_eq!(s[&(Kind::Func, "work".into())].vals, ["print hi"]);
        assert!(
            s[&(Kind::Autoload, "zmv".into())].vals.is_empty(),
            "presence only"
        );
        assert_eq!(s.len(), 7);
        assert!(parse_wire(&wire(&[&["param", "X", "scalar", "3", "only-one"]])).is_err());
    }

    #[test]
    fn diff_carries_tombstones_and_base() {
        let mut prev = parse_wire(&wire(&[
            &["param", "GONE", "scalar", "1", "x"],
            &["param", "EDITOR", "scalar", "1", "vim"],
        ]))
        .unwrap();
        let next = parse_wire(&wire(&[&["param", "EDITOR", "scalar", "1", "acme"]])).unwrap();

        let d = diff(&prev, &next);
        assert_eq!(d.len(), 2);
        let tomb = d.iter().find(|c| c.name == "GONE").unwrap();
        assert!(tomb.new.is_none() && tomb.old.as_deref() == Some(&["x".to_string()][..]));
        let edit = d.iter().find(|c| c.name == "EDITOR").unwrap();
        assert_eq!(edit.old.as_deref(), Some(&["vim".to_string()][..]));

        apply(&mut prev, &d);
        assert_eq!(prev, next, "replaying Δ reproduces the desired state");
        assert!(diff(&prev, &next).is_empty());
    }

    #[test]
    fn session_scoped_path_elements_are_dropped() {
        let mut s = parse_wire(&wire(&[&[
            "param",
            "path",
            "array-tied",
            "3",
            "/usr/bin",
            "/var/folders/x/T//cmux-cli-shims/UUID",
            "/home/m/bin",
        ]]))
        .unwrap();
        drop_volatile(&mut s, &[glob::Pattern::new("/var/folders/x/T/*").unwrap()]);
        assert_eq!(
            s[&(Kind::Array, "path".into())].vals,
            ["/usr/bin", "/home/m/bin"],
            "the doubled slash must not let it through"
        );
    }

    #[test]
    fn effect_keeps_only_what_the_bootstrap_changed() {
        let s0 = parse_wire(&wire(&[
            &["param", "HOME", "scalar", "1", "/home/m"],
            &["param", "EDITOR", "scalar", "1", "vi"],
        ]))
        .unwrap();
        let s1 = parse_wire(&wire(&[
            &["param", "HOME", "scalar", "1", "/home/m"],
            &["param", "EDITOR", "scalar", "1", "acme"],
            &["param", "MYTOKEN", "scalar", "1", "sekrit"],
        ]))
        .unwrap();
        let ignore = [glob::Pattern::new("*TOKEN*").unwrap()];
        let d = effect(&s0, &s1, &ignore);
        assert_eq!(
            d.keys().map(|k| k.1.as_str()).collect::<Vec<_>>(),
            ["EDITOR"]
        );
        assert_eq!(effect(&s0, &s1, &[]).len(), 2);
    }
}
