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

`doctor` also reports how many files arrived by process substitution
(`. <(cmd)`), which can't be hashed at all.

Under 16 KB a command is a script and gets content-hashed; above that its
symlink target, size and mtime are enough — package managers put the version
in the target (`flux -> ../Cellar/flux/2.1.0/bin/flux`), and content-hashing
your PATH would be 195 MB per reload for no extra signal.

`sharezed status` · `log` · `diff [N]` · `revert N` · `path explain` · `doctor`.
`SHAREZED_DISABLE=1` is the kill switch; `SHAREZED_IGNORE='*TOKEN* *SECRET*'`
drops keys at capture time; `SHAREZED_BOOTSTRAP` overrides `~/.zshrc`.

Local edits always win: a key you changed by hand is skipped and reported in
`sharezed status`, never clobbered. `PATH` is merged element-wise, so a local
prepend stays a prepend.

Convergence is **precmd-only, by design**: a shell catches up before its next
command, not while sitting idle at the prompt. PRD §7.5's `zle -F` path would
buy an fd and a handler per shell to close a window that ends the moment you
press enter. Not worth it.

## Not implemented yet

- `capture --from-current` (publish a live shell's state).
- Real pty capture: `zsh -f -i -c` sets `interactive`, but ZLE-only config
  (`bindkey`, `zle -N`) still fails. PRD open question 1.
- The hand-rolled startup sequence approximates zsh's own. Verified
  byte-identical to `zsh -l -i` on a real config, but `ZDOTDIR` edge cases and
  `/etc/zsh*` in unusual locations are untested.
