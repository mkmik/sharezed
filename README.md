# sharezed

direnv for *everything a shell knows*, not just env vars — capture the delta a
script makes to shell state, publish it as an append-only log, and let dozens of
long-lived zsh sessions converge on it lazily.

Design: [docs/PRD.md](docs/PRD.md).

## Install

```zsh
cargo install --path .
eval "$(sharezed hook zsh)"    # in ~/.zshrc, before direnv's hook
eval "$(direnv hook zsh)"
```

## Use

`reload` captures the **zsh startup sequence** — `.zshenv`, `.zprofile`,
`.zshrc` and everything they source — in a cleared environment. Not just
`~/.zshrc`: `~/.zshenv` is usually where `~/.cargo/bin` comes from. Nothing the
calling terminal injected can reach the capture, so reloading from two windows
publishes the same state. `SHAREZED_BOOTSTRAP` captures one file instead.

```zsh
$ vim ~/.zshrc                 # add a function, drop an alias, bump a var
$ sharezed reload
  gen 7 → gen 8: +2 functions, ~1 param, -1 alias
# every other terminal converges at its next prompt
```

`reload` never asks permission — it's your own config, and a prompt you always
answer yes to isn't a safety feature. What it does instead is *notice*. Every
sourced file is hashed and every external command the bootstrap ran is
fingerprinted, so `brew upgrade flux` shows up even though no file you own
changed:

```console
$ sharezed doctor
WARN 1 of 8 traced command(s) changed since the last publish
       ~ /opt/homebrew/bin/flux
       run `sharezed reload` to pick up what they now produce
```

Under 16 KB a command is a script and gets content-hashed; above that its
symlink target, size and mtime are enough — package managers put the version
in the target (`flux -> ../Cellar/flux/2.1.0/bin/flux`), and content-hashing
your PATH would be 195 MB per reload for no extra signal.

`reload` re-hashes what the last capture recorded and does nothing unless
something moved — no shell, **3ms against 1.1s**:

```console
$ sharezed reload
gen 5: 14 files and 10 commands unchanged
$ sharezed reload                       # after editing ~/.zsh/zmac
changed: /Users/mkm/.zsh/zmac
gen 5 → gen 6: +1 function
```

`--force` captures anyway. Reach for it when nothing a fingerprint can see
moved: a bootstrap that reads a file it never sources, or a change to
`SHAREZED_IGNORE`.

With `--silent` it prints nothing on success — errors still go to stderr — so
the pair is what belongs in a timer.

## Being told to reload

You never have to remember: the hook appends `↻ sharezed reload` to your
`RPROMPT` while a capture has something to do, and takes it back off the moment
you reload. On by default — forgetting is the failure mode it exists for.

```zsh
export SHAREZED_NO_NOTIFY=1    # if you'd rather it left your prompt alone
```

It costs one fork per prompt (2.5ms), so the steady state is no longer
fork-free; that's the price of the reminder. It strips before
it appends, so it's idempotent, it survives a prompt your config sets *after*
the hook line, and it leaves anything else in `RPROMPT` untouched. No
`promptsubst` — precmd runs before the prompt is rendered.

The check behind it is `reload --check`: exit 1 if a capture would find
something, 2.5ms, and no channel lock, since it runs on every prompt in every
shell.

If you want it to just happen instead:

```zsh
export SHAREZED_AUTORELOAD=1   # publish your own edits without typing reload
```

That one is off by default. Not for the ~6ms it adds to a prompt, which is
invisible, but because it makes pressing enter a publish action: a half-saved
zshrc reaches every shell at whatever moment you next hit a prompt.
Concurrency is safe either way — `reload` takes the channel lock before
reading meta, so eight shells racing after one edit publish exactly one
generation (verified).

Files are compared by content, so `touch` alone doesn't trigger a capture.
Adding a `source` line or a new command means editing a file that is already
tracked, so it shows up here first.

`sharezed status` · `log` · `diff [N]` · `revert N` · `path explain` · `doctor`.
`SHAREZED_DISABLE=1` is the kill switch; `SHAREZED_IGNORE='*TOKEN* *SECRET*'`
drops keys at capture time; `SHAREZED_BOOTSTRAP` captures one file instead of
the startup sequence.

Local edits always win: a key you changed by hand is skipped and reported in
`sharezed status`, never clobbered. `PATH` is merged element-wise, so a local
prepend stays a prepend.

Convergence is **precmd-only, by design**: a shell catches up before its next
command, not while sitting idle at the prompt. PRD §7.5's `zle -F` path would
buy an fd and a handler per shell to close a window that ends the moment you
press enter. Not worth it.

## Upgrading

Three namespaces, one rule each:

| | synced | on conflict |
|---|---|---|
| `_sharezed_*` — sharezed's own hook | yes | theirs-wins |
| `SHAREZED_*` — configuration you wrote | yes | ours-wins |
| cursor, conflicts, channel, `$path` at install | never | — |

So a new hook reaches running shells through the ordinary apply path, with no
separate mechanism. Its functions are applied last in an entry, so the guards
for everything else run on one consistent version of the machinery. Theirs-wins
is what makes it work at all: a shell holding the hook its own zshrc installed
would otherwise read as a local edit and keep an old hook forever.

Shells started before this existed need one nudge, then they're autonomous:

```zsh
eval "$(sharezed hook zsh)"
```

## Not implemented yet

- `capture --from-current` (publish a live shell's state).
- Real pty capture: `zsh -f -i -c` sets `interactive`, but ZLE-only config
  (`bindkey`, `zle -N`) still fails. PRD open question 1.
- The hand-rolled startup sequence approximates zsh's own. Verified
  byte-identical to `zsh -l -i` on a real config, but `ZDOTDIR` edge cases and
  `/etc/zsh*` in unusual locations are untested.
