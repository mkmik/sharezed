# sharezed — PRD

**Codename:** sharezed
**Status:** Draft v3 for review
**One-liner:** direnv for *everything a shell knows*, not just env vars — capture the delta a script makes to shell state, publish it as an append-only log, and let dozens of long-lived zsh sessions converge on it lazily.

> **v3 changes:** scope narrowed to four kinds — environment variables, non-exported shell variables, aliases, functions. Options, named directories, key bindings, and completions are now explicit non-goals (§3); finding §5.4(b) retires with them.
>
> **v2 changes:** secrets descoped entirely (§3). Ordered-list merge promoted to a first-class design section with a verified algorithm (§8). One correction to v1's tied-parameter rule — `PATH` must be applied on the **array** side, not the scalar (§5.4c).

---

## 1. Problem

The working setup is many zsh sessions across many terminals, most alive for days. direnv solves part of the problem — change a value in one file, every session picks it up before its next command. Two gaps remain:

1. **Env vars only.** Shell (non-exported) variables, functions, and aliases don't propagate. Editing `~/.zshrc` means either restarting every terminal or `source ~/.zshrc` in each one by hand.
2. **Manual curation.** You have to decide, in advance, which vars to share and write them down. Tools that "just append an init snippet to your zshrc and tell you to reload" are invisible to direnv.

The insight: instead of *enumerating* what to share, **observe** what a script does to a shell and share the observation.

## 2. Core model

```
  clean shell ──capture──▶ S₀
       │
    source ~/.zshrc (or any bootstrap)
       │
       └──capture──▶ S₁
```

`S₁` is the **desired state**. The log entry published at generation *n* is:

> `Δₙ = diff(Sₙ₋₁, Sₙ)` — the diff between the **previous published desired state** and the new one.

This is the most important design decision in the document, so it's worth being explicit about why it isn't `diff(S₀, S₁)`:

Publishing `diff(S₀, S₁)` means a consumer that already applied generation 1 and now applies generation 2 keeps everything generation 1 added, even if you *deleted* it from your zshrc. Removals never propagate and shells accumulate ghosts. Diffing successive *desired states* makes deletions first-class: if `Sₙ` no longer has function `foo`, `Δₙ` carries a tombstone for `foo`.

Consumers replay `Δ` entries in order. Each shell tracks a **cursor** (last applied sequence number) and catches up on its next prompt.

```
producer:  zsh -f ──▶ capture ──▶ diff vs Sₙ₋₁ ──▶ append Δₙ ──▶ bump head
                                                          │
consumer:  precmd hook ── head != cursor? ──▶ apply Δ(cursor..head] ──▶ cursor = head
```

## 3. Goals / Non-goals

### Goals
- G1 — Propagate the four kinds in scope — **environment variables, non-exported shell variables, aliases, and functions** — to every subscribed live shell without restarting them.
- G2 — Propagate **removals**, not just additions.
- G3 — No manual enumeration for the common case. `sharezed reload` after editing zshrc is the whole workflow.
- G4 — Never clobber state a shell's owner set by hand. Local edits win by default.
- G5 — **Ordered-list params (`PATH` above all) merge correctly, and a local prepend stays a prepend.**
- G6 — Sub-millisecond, **fork-free** cost on the steady-state prompt (nothing to apply).
- G7 — A bad diff must not brick every terminal at once. Blast radius is the main risk in this design.

### Non-goals
- **Secrets.** Explicitly out of scope. sharezed stores values as they are; if a value shouldn't be on disk, don't sync it. A `SHAREZED_IGNORE` pattern list (§7.2) is the escape hatch — matched keys are dropped at capture time and never enter the log. Anything more (agent, sealing, lazy refs) is a separate product.
- Syncing cwd, job table, fds, history, ZLE editing state, terminal modes.
- Cross-user or cross-host sync. Single uid, single machine.
- bash/fish. (Architecture shouldn't preclude them; the capture layer is shell-specific by design.)
- Replacing direnv. sharezed is machine/user-scoped; direnv stays directory-scoped. See §12.
- **Everything outside the four kinds in G1.** Specifically out: shell options (`setopt`), named directories (`hash -d`), key bindings (`bindkey`), traps, and the completion *system* — `_patcomps`, `_services`, the `$fpath` stubs `compinit` autoloads, and the ~1900 `_comps` entries it builds from their `#compdef` tags, all of which every shell rebuilds for itself. A completion your zshrc *writes* is not that, and does sync: the `_ccwt` that `ccwt init zsh` defines is an ordinary function, one of the four kinds, and the `compdef` binding it to a command rides along as a fifth. The filter is "does this `_comps` entry point at a function we are carrying" — 5 of 1874 on a real zshrc. Options in particular were considered and cut: they are the highest-risk category (a stray `setopt` mode change is far harder to notice and diagnose than a wrong variable) for the least benefit.

## 4. Terminology

| Term | Meaning |
|---|---|
| **State (S)** | Serialized snapshot of transferable shell state at a point in time |
| **Generation** | A published desired state, numbered monotonically |
| **Δ / entry** | Diff between consecutive generations; the unit appended to the log |
| **Head** | Sequence number of the newest entry |
| **Cursor** | Per-shell sequence number of the last applied entry |
| **Channel** | A named stream of entries; shells subscribe to one or more |
| **Base** | Per-shell record of what sharezed last *installed* — the merge base |
| **Tombstone** | An entry record meaning "this key should no longer exist" |

---

## 5. What is "shell state"

Three buckets. Getting this taxonomy right is most of the product.

### 5.1 Transferable (v1 scope)

| Kind | Source | Apply verb |
|---|---|---|
| Scalar / int / float params | `$parameters` + `${(P)n}` | `typeset -g[x]` |
| Array params | `$parameters` + `${(@P)n}` | `typeset -ga` |
| Associative params | `$parameters` + `${(@kvP)n}` | `typeset -gA` |
| Functions | `$functions` (assoc: name → body) | `eval 'name() { … }'` |
| Autoload stubs | `$functions` body == `builtin autoload -X*` | `autoload -Uz name` (after `fpath`) |
| Aliases / global / suffix | `$aliases`, `$galiases`, `$saliases` | `alias`, `alias -g`, `alias -s` |

The `zsh/parameter` module is the whole capture mechanism. Everything above is an associative array, so capture is a loop and diff is a map comparison — no parsing of `typeset -p` output, no quoting bugs.

Two things that look like extra categories but aren't. **Autoload stubs** are just entries in `$functions` whose body is `builtin autoload -X…` rather than real code, so they ride along with functions for free — they only need `fpath` applied first (§8.7). And **`fpath` itself is an ordinary non-exported array**, so it is already covered by "shell variables"; it needs no special case beyond ordering.

### 5.2 Per-shell — never touch

`PWD`, `OLDPWD`, `SHLVL`, `$$`, `PPID`, `TTY`, `TTYIDLE`, `_`, `?`/`status`, `pipestatus`, `RANDOM`, `SECONDS`, `EPOCHSECONDS`, `LINENO`, `HISTCMD`, `funcstack`, `functrace`, `funcfiletrace`, `ZSH_SUBSHELL`, `ZSH_EVAL_CONTEXT`, `ZSH_SCRIPT`, `ZSH_ARGZERO`, `ZSH_EXECUTION_STRING`, `COLUMNS`/`LINES`, terminal and job-control state.

### 5.3 Dangerous — explicit denylist

- `TRAPEXIT` and friends — propagating an exit trap into 30 shells is a footgun.
- Readonly params — assignment silently fails (verified: non-zero return, no output). Detect via the `readonly` attribute and skip with a warning.
- Anything in the `sharezed` namespace (`SHAREZED_*`, `_sharezed_*`) — the tool must not sync itself.

### 5.4 ⚠️ Verified findings that change the design

All confirmed against zsh 5.9. Each would have silently broken a naive implementation.

**(a) "Skip special params" is too aggressive — `PATH` is special.**

```
PATH -> scalar-tied-export-special
path -> array-tied-unique-special      # with `typeset -U path`
PWD  -> scalar-export                  # NOT flagged special!
```

Neither `special` nor `!special` is the right filter. The rule: **denylist by name (§5.2) + skip `readonly`/`local` + an explicit allowlist of special params worth syncing** (`PATH`, `FPATH`, `MANPATH`, `CDPATH`, `LD_LIBRARY_PATH`, prompt params if opted in).

**(b) ~~`emulate -L zsh` masks the caller's options~~ — retired by the scope cut.**

Measured and real (`$options[extendedglob]` reads `off` after `emulate -L zsh` when the caller had it `on`), but it only bites a harness that captures `$options`, which this one no longer does. `emulate -L zsh` does not perturb `$parameters`, `$functions`, or `$aliases`, so the harness can keep it for its own hygiene. Recorded because the general lesson still applies — **capture before you perturb** — and because it becomes load-bearing again the moment options come back into scope.

**(c) Tied params: apply the scalar side — EXCEPT for `path`. ⟵ correction to v1.**

v1 said "apply the scalar side only; the array follows." True for propagation, but:

```
typeset -U path
PATH=/a:/b:/a:/c   →  path=(/a /b /a /c)    # duplicates SURVIVE
path=(/a /b /a /c) →  path=(/a /b /c)       # -U applies
```

**`typeset -U` is not enforced on scalar assignment.** For list-typed params the array side is also strictly nicer: no colon-splitting, no empty-component ambiguity, and the merge in §8 works on elements anyway. Rule: **list params → assign the array side; other tied scalars → assign the scalar side.**

Related, all verified: `-U` keeps the **first** occurrence (matching PATH's first-match-wins semantics); empty components are real array elements and are preserved; `~` is expanded at array-assignment time; and `/a`, `/a/`, `/a//` are **three distinct elements** even under `-U` — which is why §8 needs lexical normalization.

**(d) `typeset` inside a function is local; bare assignment is not.** `FOO=1` inside a function creates a global; `typeset FOO=1` creates a local. The payload uses `typeset` to carry attributes and the precmd hook *is* a function, so **every generated `typeset` must use `-g`**. Not optional.

---

## 6. Product surface

```
sharezed reload [--channel NAME]     # clean-room: capture, source bootstrap, capture, publish Δ
sharezed capture --from-current      # publish delta of *this* live shell vs its base
sharezed status                      # cursor vs head, pending entries, conflicts, path notes
sharezed diff [N]                    # human-readable view of an entry
sharezed export zsh                  # emit apply payload for current shell (the eval'd thing)
sharezed path explain                # show how this shell's PATH was merged, entry by entry
sharezed log [--channel NAME]        # list generations
sharezed revert [N]                  # publish the inverse of entry N
sharezed allow                       # trust a changed bootstrap script (direnv-style)
sharezed hook zsh                    # print shell integration; eval in .zshrc
sharezed doctor                      # diagnose hook order, perms, stale cursors, dead PATH dirs
```

**Kill switch:** `SHAREZED_DISABLE=1` in the environment makes the hook a no-op immediately, in that shell, with no daemon involvement. Required for G7.

### Primary workflow
```zsh
$ vim ~/.zshrc          # add a function, remove an alias, bump a var
$ sharezed reload
  gen 7 → gen 8: +2 functions, ~1 param, ~PATH (+1 -1), -1 alias
  published to channel 'user' — 14 shells will converge
# every other terminal picks it up at its next prompt
```

## 7. Design

### 7.1 Producer: clean-room capture

`sharezed reload` spawns a **fresh** `zsh -f` (no rcs), captures `S₀`, sources the configured bootstrap, captures `S₁`. Reproducible and free of contamination from whatever the interactive session accumulated.

**Open problem — pty.** A non-interactive zsh takes different code paths: `[[ -o interactive ]]` guards skip, ZLE is unavailable so `bindkey`/`zle -N` fail, `compinit` behaves differently. Options:

- **(A) `zsh -i` on a pty** spawned by the host process. Highest fidelity, most machinery; capture output must go to fd 3 or a temp file, not stdout.
- **(B) `zsh -f -c` non-interactive.** Simple, misfires on ZLE-dependent config.
- **(C) `zsh/zpty`** — zsh's own pseudo-tty module; avoids writing pty code in the host language.

**Recommendation: (C) for the spike, (A) for v1.** Half the interesting zshrc content is interactive-only.

A secondary producer mode, `--from-current`, diffs a live shell against its base — for "I hand-tweaked this session, share it." Noisier; the §5.2 filters do real work there.

### 7.2 Wire format

Capture emits **NUL-delimited records** on a dedicated fd. Verified working:

```
kind \0 name \0 meta \0 nvals \0 val₁ \0 … \0 valₙ \0
```

`kind ∈ {param, func, alias, galias, salias}`. For associations, `nvals` is `2k` with alternating key/value. NUL delimiting sidesteps every quoting question — zsh params cannot contain NUL, so it's unambiguous.

`SHAREZED_IGNORE` (glob list, from config) is applied here: matched keys are dropped at capture and never reach the log. This is the only privacy mechanism in scope.

The host process (**Rust** — see §7.2.1) parses this into a typed state map. Entries are stored as structured records (JSONL for auditability, CBOR if size bites), **not** as shell code. The tool generates shell code at apply time, doing its own quoting. `sharezed diff` stays readable and the log isn't a raw code-execution blob.

#### 7.2.1 Decision: host language is Rust

Three languages appear in this document and only one is a choice. **zsh** for the capture harness and hook is forced by the problem. **Python** in §8.8 is reference-only, for specifying behaviour and testing against §8.5. The host binary is the open decision, and startup latency was the stated concern — so it was measured rather than assumed.

Benchmark: identical anchored-rebase implementations in both languages (verified byte-identical output), 50 KB entry in, 59 KB payload out, `fork`+`execv`+`waitpid` driven from a C harness to keep the shell out of the measurement, median of 9 runs × 300–400 iterations.

| Binary | Linkage | Pure startup | Realistic 50 KB apply |
|---|---|---:|---:|
| Go | static, **no libc** | **740 µs** | **1229 µs** |
| Rust | static glibc | 1049 µs | 1260 µs |
| Rust | dynamic glibc | 1335 µs | 1561 µs |
| C (control) | static glibc | 947 µs | — |
| C (control) | dynamic glibc | 1058 µs | — |

Findings:

1. **Rust is not faster to start.** Static Rust loses to Go by ~310 µs on pure startup.
2. **But the cause is libc, not the language.** A Go binary that touches no libc has no dynamic section at all (verified). Static-glibc **C** also loses to Go (947 µs vs 740 µs) — Go beats hand-written C purely by avoiding glibc's startup work (IRELATIVE/ifunc resolution, locale, TLS setup). Rust's *own* runtime overhead is small: ~100 µs over static C, ~280 µs over dynamic C.
3. **Static linking is worth ~290 µs** — a bigger lever than the language choice.
4. **On real work the gap collapses to ~31 µs (2.5%).** Once you're parsing 50 KB and running an LCS merge, the runtime-init difference is noise.
5. **Untested: Rust + musl.** The musl std wasn't installable in the benchmark environment. musl's startup is far leaner than glibc's, and this is the standard configuration for fast-start Rust CLIs — it would likely close most or all of the remaining gap. Build with `--target x86_64-unknown-linux-musl`.

Calibration: `fork`+`exec` alone costs ~900 µs in the 1-vCPU container used here; on real hardware it's ~150–300 µs and every number above compresses several-fold. Against the §9 budget of **15 ms** for the apply path, every variant is 10× under. And per §7.5 the steady-state path **forks zero times** — the binary does not execute at all when there is nothing to apply, so startup cost is paid only on the handful of occasions per day when a generation actually lands.

**Decision: Rust, statically linked against musl.** The measured penalty is ~30 µs on the workload that matters, incurred a few times a day, against a budget it clears by an order of magnitude. Maintainer familiarity dominates a difference that small.

Caveats on the numbers: rustc 1.75 / Go 1.22, single vCPU, static glibc rather than musl. The ordering is robust; the absolute values are container-inflated.

For diffing, compare a hash per key rather than full values — function bodies dominate size (a bare `zsh -f` state is ~12 KB; an oh-my-zsh-class setup is comfortably 1–3 MB across thousands of functions).

### 7.3 The log

```
$XDG_STATE_HOME/sharezed/<channel>/
  head                    # single line: current seq. Read fork-free by the hook.
  snapshot-000012.jsonl   # full desired state at gen 12
  000013.jsonl            # Δ entries after the snapshot
  000014.jsonl
  meta.json               # bootstrap script hash, trust state, config
```

- **Append-only, monotonic seq.** Publish: write entry to temp, `fsync`, `rename` into place, then atomically rewrite `head`. `head` is written last so a shell never sees a seq whose entry isn't durable.
- **Compaction via snapshots.** Every K entries (or T bytes) write a full-state snapshot. A shell whose cursor predates the oldest retained entry applies the *snapshot* and jumps to that seq.
- **No liveness tracking needed.** Because falling arbitrarily far behind is always recoverable via snapshot, there's no need to know which shells are alive or to refcount entries. GC is "keep the newest snapshot and everything after it."

### 7.4 Consumer: apply and the three-way merge (G4)

Per key, sharezed has:

- **base** — what sharezed last installed in *this* shell (per-shell, `typeset -gA SHAREZED_BASE`, storing a **hash** not the value)
- **ours** — the key's current value in this shell
- **theirs** — the incoming value

| Condition | Action |
|---|---|
| `hash(ours) == base` | Fast-forward: apply `theirs`, update base |
| key absent, no base | Apply `theirs` (new key) |
| `hash(ours) != base` | **Conflict.** Local edit wins: skip, record, surface in `sharezed status` |
| tombstone, `hash(ours) == base` | `unset` / `unfunction` / `unalias`, drop base entry |
| tombstone, `hash(ours) != base` | Skip — user redefined it locally |

Policy is per-key configurable (`theirs-wins` for a var you always want authoritative), but **`ours-wins` is the default**. A tool that silently reverts your ad-hoc `export DEBUG=1` gets uninstalled within a day.

**Ordered-list params bypass this table entirely** and use the element-wise algorithm in §8. Whole-value hashing is wrong for `PATH`: a shell that prepended one directory would conflict on every single generation, forever.

### 7.5 Notification and latency (G6)

**Steady state must not fork.** The hook reads `head` with pure builtins:

```zsh
_sharezed_precmd() {
  [[ -n $SHAREZED_DISABLE ]] && return 0
  local head
  read -r head < $SHAREZED_HEAD 2>/dev/null || return 0   # no fork (verified)
  [[ $head == $SHAREZED_CURSOR ]] && return 0
  _sharezed_apply "$head"                                  # forks once, only when behind
}
```

One `open`+`read` on an ~8-byte file. Target **<200 µs** when up to date — a 10–100× improvement on direnv's per-prompt cost, and the difference between "invisible" and "my prompt feels laggy."

**Phase 2 — apply while idle at the prompt.** The stated constraint is that shells only wake on pre/post-command hooks, so a shell sitting idle for hours stays stale. `zle -F` fixes this: register a descriptor and zsh runs a handler *while sitting in ZLE*.

```zsh
_sharezed_zle_ready() { _sharezed_apply; zle reset-prompt; }
exec {SHAREZED_FD}< $SHAREZED_FIFO
zle -F $SHAREZED_FD _sharezed_zle_ready
```

Use a FIFO rather than process substitution — the latter leaves a child process per shell, which at 30 terminals is real. This turns "converges by next command" into "converges in milliseconds."

### 7.6 Scoping: channels

Not every diff belongs in every shell. A channel is a named log; shells subscribe via `SHAREZED_CHANNELS=(user work)`. Defaults: `user` (global, from `~/.zshrc`) plus optional per-project channels.

### 7.7 Trust model

The log is a **code-execution channel into every shell you own**.

- Store under 0700; verify uid and mode on every read, refuse otherwise. Never apply an entry written by another uid.
- **direnv-style approval:** if the bootstrap script's content hash changed since last publish, `sharezed reload` requires `sharezed allow`. You already have this muscle memory.
- `sharezed diff` before `allow` shows exactly what will land in 30 shells.

### 7.8 Failure handling (G7)

The nightmare: one bad publish simultaneously breaks every terminal.

1. **Publish-time validation.** Before appending, evaluate the generated payload in a sandbox `zsh -f -c`. Non-zero exit or stderr output ⇒ refuse to publish.
2. **Transactional apply with rollback.** The consumer knows the exact key set an entry touches. Snapshot those keys before eval; on failure restore and abort. Bounded, cheap.
3. **Quarantine.** If apply fails N times in a shell, that shell self-disables, prints a one-line notice with the seq, and stops trying. A poison entry costs a message, not a dead prompt.
4. **`sharezed revert N`** publishes the inverse entry — recovery is a normal publish, converging the same way.
5. **`SHAREZED_DISABLE=1`** — instant, local, no daemon.
6. **Never redefine sharezed's own functions mid-apply.** Namespace-exclude and assert.

---

## 8. Ordered-list merge: `PATH` (G5)

`PATH` is the hard case and deserves its own algorithm. It is neither a set nor an opaque scalar: it is a **priority-ordered list where position is meaning**. Earlier wins. And the dominant real-world mutation is the **prepend** — `nvm use`, `pyenv shell`, `rustup override`, a venv activate, direnv, or just `path=(~/proj/bin $path)` typed by hand. A prepend is not "an addition"; it is an assertion of priority, and a merge that preserves the entry but drops it to the tail has silently done the wrong thing.

### 8.1 Why the obvious approaches fail

| Approach | Failure |
|---|---|
| Whole-string 3-way | Conflicts on *every* generation for any shell that ever prepended anything. Useless. |
| Treat as a set, union | Loses order — the entire semantics of `PATH`. |
| `theirs ++ (ours − theirs)` | Local prepends fall to the tail and get shadowed. Silent, and painful to debug. |
| `(ours − theirs) ++ theirs` | Local *appends* get promoted to top priority. Equally wrong, opposite direction. |
| Line-based diff3 | Produces conflict markers. There is no human to resolve them at precmd time. |

### 8.2 Model: anchored rebase

Treat the local difference as a **patch expressed relative to anchors**, then rebase that patch onto the incoming list. The key move:

> Every locally-added element is recorded together with its **nearest surviving predecessor** — the closest element to its left that also exists in the base. An element with *no* surviving predecessor is, by definition, at the head.

**Prepend-ness is therefore derived, not special-cased.** "Predecessor = HEAD" *is* what a prepend is, and it survives arbitrary upstream reordering, insertion, and deletion. No config, no heuristic, no marker files.

The algorithm, given `B` (base — what sharezed last installed here), `O` (ours — current `$path`), `T` (theirs — incoming):

1. **Normalize & dedup.** Lexical only: collapse `//`, strip trailing `/` (never `realpath` — symlink resolution changes meaning and costs syscalls). Dedup keeping **first**, matching zsh `-U`. Normalization is for *comparison*; the original string is what gets emitted.
2. **Anchors = LCS(B, O).** Elements present in both in the same relative order. Everything in `O` outside the LCS is a local addition; everything in `B` outside it is a local deletion.
3. **Record each local addition** with its nearest preceding anchor, or `HEAD` if none.
4. **Start from `T`,** minus anything the user locally deleted (their removal is a local edit; ours-wins). Note it if `T` still wants it.
5. **Rebase each local addition** back on, walking in reverse so multiple head-prepends keep their relative order:
   - already in `T`, and its anchor was `HEAD` → **hoist to head** (you prepended it; that was a priority assertion, honor it)
   - already in `T`, otherwise → leave `T`'s placement
   - anchor is `HEAD` → insert at index 0
   - anchor survives in `T` → insert immediately after it
   - anchor vanished → insert at head, flag as an **orphan** in `sharezed status`
6. **Dedup keeping first.** Done.

### 8.3 The base rule (subtle, load-bearing)

After the merge, set `base := T` — the **pure incoming list**, *not* the merged result.

If base were the merged result, local prepends would be absorbed into "what sharezed installed," look managed on the next generation, and get wiped. Keeping `base = T` means the local delta is recomputed correctly and identically every generation, which is exactly what makes the operation **idempotent** (scenario 3 below).

### 8.4 Family eviction — the `nvm use` problem

Anchored rebase alone has one bad failure mode. `nvm use 18` prepends `~/.nvm/versions/node/v18/bin`. Six generations later that entry is *still* pinned at the head, shadowing the v20 that the bootstrap now installs. A permanently stale prepend is worse than no merge at all.

Fix: declare mutually-exclusive **families**. A newer member arriving from `T` evicts older local members.

```toml
[[path.family]]
match = "~/.nvm/versions/node/*/bin"
[[path.family]]
match = "~/.pyenv/versions/*/bin"
[[path.family]]
match = "~/.rustup/toolchains/*/bin"
```

Ships with defaults for nvm/pyenv/rbenv/rustup/asdf/JDK switchers; user-extensible. This is the one place the algorithm needs configuration, and it's the right place for it.

### 8.5 Verified behaviour

A reference implementation was run against 12 scenarios; all pass. Ordering as produced by the algorithm:

| # | Scenario | Result |
|---|---|---|
| 1 | Local prepends + upstream inserts a new middle entry | prepends stay at head, insert lands in place |
| 2 | Three local prepends | relative order among them preserved |
| 3 | Re-apply the same generation | **no change** (idempotent) |
| 4 | Local *append* | stays at the tail — not promoted |
| 5 | Local mid-list insert, upstream fully reverses order | insert follows its anchor |
| 6 | User deleted a managed entry | stays deleted; noted that upstream still wants it |
| 7 | User prepended something upstream already has lower down | **hoisted** to head, noted |
| 8 | Upstream drops an entry the user never touched | dropped |
| 9 | Local insert whose anchor vanished upstream | placed at head, flagged **orphan** |
| 10 | `/opt/x/` locally vs `/opt/x//` upstream | recognised as the same entry, no duplicate |
| 11 | nvm v18 local prepend, v20 arrives upstream | v18 **evicted**, v20 at head |
| 12 | Empty component (`::`, meaning cwd) | preserved verbatim |

Every non-trivial resolution (hoist, orphan, eviction, kept-deletion) emits a note. `sharezed path explain` renders them, so a surprising `PATH` is always traceable to a specific rule rather than to vibes.

### 8.6 Applying the result

Per §5.4(c): assign the **array** side.

```zsh
typeset -gaU path=( '/opt/nvm/v20/bin' '/home/m/bin' '/usr/local/bin' '/usr/bin' '/bin' )
```

`PATH` follows automatically via the tie, `-U` is genuinely enforced (unlike on scalar assignment), and colons and empty components never need escaping.

### 8.7 Scope

The same machinery serves every ordered-list param: `fpath`, `manpath`, `cdpath`, `infopath`, `LD_LIBRARY_PATH`, `PKG_CONFIG_PATH`, `GOPATH`-style lists. Configured by name:

```toml
list_params = ["path", "fpath", "manpath", "cdpath", "infopath"]
list_params_scalar = ["LD_LIBRARY_PATH", "PKG_CONFIG_PATH"]   # split on ':'
```

`fpath` deserves a note: it must be merged and applied **before** any autoload stub in the same entry, or the stubs resolve against a stale search path.

### 8.8 Reference implementation

```python
def merge(base, ours, theirs, families=()):
    base, ours, theirs = dedup(base), dedup(ours), dedup(theirs)
    anchors = lcs_anchors(base, ours)          # indices into `ours` surviving from `base`
    tset    = {norm(x) for x in theirs}
    notes   = []

    # local additions, each tagged with its nearest surviving predecessor (None == HEAD)
    local = []
    for i, e in enumerate(ours):
        if i in anchors: continue
        pred = next((norm(ours[j]) for j in range(i-1, -1, -1) if j in anchors), None)
        local.append((e, pred))

    removed = {norm(x) for x in base} - {norm(x) for x in ours}
    result  = [x for x in theirs if norm(x) not in removed]

    for pat in families:                        # §8.4
        if any(fnmatch(norm(x), pat) for x in result):
            kept = [(e, p) for e, p in local if not fnmatch(norm(e), pat)]
            for e, _ in set(local) - set(kept):
                notes.append(f"evicted: {e} (superseded in family {pat})")
            local = kept

    for e, pred in reversed(local):             # reverse ⇒ head-prepends keep their order
        ne = norm(e)
        if ne in tset:
            if pred is None:                    # you prepended it ⇒ priority assertion
                result = [x for x in result if norm(x) != ne]
                result.insert(0, e)
                notes.append(f"hoisted: {e}")
            continue
        if pred is None:
            result.insert(0, e)
        else:
            idx = next((k for k, x in enumerate(result) if norm(x) == pred), None)
            if idx is None:
                result.insert(0, e); notes.append(f"orphan: {e} (anchor {pred} gone)")
            else:
                result.insert(idx + 1, e)

    return dedup(result), notes                 # caller then sets base := theirs  (§8.3)
```

`norm` is lexical-only; `dedup` keeps first; `lcs_anchors` is `SequenceMatcher`-equivalent. `n` is 20–60 in practice, so O(n²) LCS is ~3600 operations — free. Membership-based anchoring (skip the LCS) is a valid simplification but mishandles the case where a user *moves* an existing entry to the front; LCS gets that right, treating it as delete-plus-prepend, which produces exactly the intended hoist.

### 8.9 Bootstrapping the base

A shell that starts before ever applying an entry has no base. Two options:

- **base := ∅** → all of `$path` looks locally-added and anchored at HEAD; result is `O ++ T`. Safe but noisy.
- **base := `$path` at hook-install time** (recorded as `SHAREZED_PATH0` in `sharezed hook zsh`) → subsequent local prepends are correctly identified as local. **Preferred.**

### 8.10 Hygiene, deliberately off the hot path

Long-lived prepends rot — deleted worktrees, removed venvs, uninstalled toolchains. Pruning entries whose directory no longer exists requires stat-ing N dirs, which does not belong in a precmd hook. Expose it as `sharezed doctor --prune-missing` and as a publish-time report, never as an automatic step.

---

## 9. Performance budget

| Path | Target | Notes |
|---|---|---|
| precmd, up to date | < 200 µs, **0 forks** | single small read |
| precmd, one small entry | < 15 ms | 1 fork + eval |
| PATH merge | < 1 ms | n ≈ 40, O(n²) LCS |
| Catch-up from snapshot | < 150 ms | rare, after long sleep |
| `sharezed reload` | < 2 s | dominated by sourcing zshrc twice |
| Log entry size | < 50 KB typical | hash-based diff; bodies only when changed |

## 10. Milestones

**M0 — Spike.** Params only, one channel, poll-only. Capture via `zsh/zpty`, NUL wire format, JSONL log, naive whole-value overwrite. Exit criterion: edit a var in zshrc, run `reload`, watch three terminals converge. *Largely validated already — a prototype harness produced a correct diff (`+alias gs`, `+func work`, `+MYTOKEN`, `+EDITOR`) on the first run.*

**M1 — PATH.** §8 in full: normalization, LCS anchoring, rebase, base-is-theirs, families, `path explain`, the §5.4(c) array-side apply. Worth doing early and standalone — it's the piece most likely to make or break daily use, and it's independently testable against the §8.5 table.

**M2 — Correctness.** Functions, autoload stubs, aliases, tombstones, three-way merge with conflict reporting, snapshots + compaction, `status`/`diff`/`log`, rollback + quarantine, the §5.4 filters.

**M3 — Ergonomics.** `zle -F` instant apply, channels, trust/`allow`, `revert`, `doctor`, direnv interop ordering.

**M4 — Long tail.** `--from-current` producer, bash support if it still seems like a good idea. Any scope expansion beyond the four kinds is a deliberate decision to revisit here, not a drift.

## 11. Open questions

1. **pty fidelity vs complexity** — is `zsh/zpty` sufficient, or does v1 need a real pty in the host process? Decide during M0; biggest unknown.
2. **Hoisting (§8.2 step 5, case 1)** — is "you prepended it, so it wins over upstream's placement" always right? It's the correct reading of intent, but it means a locally-prepended entry can outrank upstream's ordering forever. Alternative: hoist, but expire after N generations. Needs real-world use to settle.
3. **Prompt params** — sync `PS1`/`RPROMPT`? Powerful, but a broken prompt in 30 terminals at once is the worst possible failure. Lean: opt-in only, validated hard at publish.
4. **Function provenance filtering** — `$functions_source` reports the defining file (verified). Should `--from-current` sync only functions sourced from the bootstrap tree, ignoring interactively-defined ones? Probably yes.
5. **Multiple producers** — two `reload`s racing. Simple answer: publish takes an `flock` on the channel. Confirm that's sufficient.

## 12. Interop with direnv

They compose rather than compete, but **hook order matters**. sharezed is user/machine-scoped; direnv is directory-scoped and more specific, so direnv must win:

```zsh
eval "$(sharezed hook zsh)"   # registers precmd first
eval "$(direnv hook zsh)"     # runs after → directory scope overrides
```

`sharezed doctor` verifies this ordering and warns loudly if inverted. Additionally, sharezed should read `DIRENV_DIFF` and treat direnv-owned keys as locally-modified — always conflict, never clobber. For `PATH` specifically this matters a lot: direnv's own prepends should be classified as local additions anchored at HEAD, which §8 already does correctly without special-casing, provided sharezed merges *before* direnv runs.

---

## Appendix A — verified capture harness

Runs on zsh 5.9. Not production quality (missing the §5.4 allowlist and the `emulate` fix); it exists to show the mechanism is real.

```zsh
#!/usr/bin/env zsh
emulate -L zsh                      # safe: nothing captured here is option-sensitive (§5.4b)
zmodload zsh/parameter

typeset -ga SZ_DENY=(PWD OLDPWD SHLVL _ RANDOM SECONDS EPOCHSECONDS LINENO
                     HISTCMD funcstack functrace funcfiletrace ZSH_SUBSHELL
                     TTY TTYIDLE ZSH_EVAL_CONTEXT status pipestatus
                     ZSH_SCRIPT ZSH_ARGZERO ZSH_EXECUTION_STRING)

emit() { print -rN -- "$@" }        # NUL-separated

local name attrs
for name attrs in ${(kv)parameters}; do
  [[ -n ${SZ_DENY[(r)$name]} ]] && continue
  [[ $name == (SHAREZED_*|_sharezed_*) ]] && continue
  [[ $attrs == *readonly* || $attrs == *local* ]] && continue
  [[ $attrs == *special* ]] && ! _sz_allowed_special $name && continue   # §5.4(a)
  case $attrs in
    association*) emit param "$name" "$attrs" "${#${(Pk)name}}" "${(@kvP)name}" ;;
    array*)       emit param "$name" "$attrs" "${#${(@P)name}}"  "${(@P)name}"  ;;
    *)            emit param "$name" "$attrs" 1 "${(P)name}" ;;
  esac
done

for name in ${(k)functions}; do
  emit func "$name" "${functions_source[$name]:-}" 1 "${functions[$name]}"
done
for name attrs in ${(kv)aliases};  do emit alias  "$name" "" 1 "$attrs"; done
for name attrs in ${(kv)galiases}; do emit galias "$name" "" 1 "$attrs"; done
for name attrs in ${(kv)saliases}; do emit salias "$name" "" 1 "$attrs"; done
```

Note the tie handling differs from v1: list params are emitted on the **array** side (`path`, not `PATH`) per §5.4(c), and the scalar twin is suppressed at diff time.

Autoload stubs are identifiable by body — `${functions[zmv]}` is `builtin autoload -XU`, not the real body — so they're emitted as `autoload -Uz name` on apply, after `fpath`.

## Appendix B — apply payload shape

Generated by the tool, `eval`'d by the hook. Every `typeset` carries `-g` (§5.4d).

```zsh
# entry 14, channel 'user'
typeset -gx EDITOR='acme'
typeset -gaU path=( '/opt/nvm/v20/bin' '/home/m/bin' '/usr/local/bin' '/usr/bin' '/bin' )
work() {
	print "working in $1"
}
alias gs='git status'
unfunction old_helper 2>/dev/null               # tombstone
unset STALE_VAR 2>/dev/null                     # tombstone
typeset -gA SHAREZED_BASE=( "${(@kv)SHAREZED_BASE}"
                            EDITOR 'a1b2…' work 'c3d4…'
                            path '/usr/local/bin:/usr/bin:/bin' )   # base := THEIRS, §8.3
SHAREZED_CURSOR=14
```

Note that `SHAREZED_BASE[path]` records the *incoming* list, not the merged one. The merged list is what's live in `$path`; the difference between the two is precisely this shell's local prepends, recomputed from scratch on the next generation.

Conflicted keys are simply absent from the payload — the tool decided against them at generation time using the base hashes the shell reported.