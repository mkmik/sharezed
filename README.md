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

```zsh
$ vim ~/.zshrc                 # add a function, drop an alias, bump a var
$ sharezed reload --allow
  gen 7 → gen 8: +2 functions, ~1 param, -1 alias
# every other terminal converges at its next prompt
```

`sharezed status` · `log` · `diff [N]` · `revert N` · `path explain` · `doctor`.
`SHAREZED_DISABLE=1` is the kill switch; `SHAREZED_IGNORE='*TOKEN* *SECRET*'`
drops keys at capture time; `SHAREZED_BOOTSTRAP` overrides `~/.zshrc`.

Local edits always win: a key you changed by hand is skipped and reported in
`sharezed status`, never clobbered. `PATH` is merged element-wise, so a local
prepend stays a prepend.

## Not implemented yet

- `zle -F` instant apply — convergence happens at the next prompt, not while idle.
- `capture --from-current` (publish a live shell's state).
- Real pty capture: `zsh -f -i -c` sets `interactive`, but ZLE-only config
  (`bindkey`, `zle -N`) still fails. PRD open question 1.
- `reload` inherits the calling shell's `PATH` into the clean room, so local
  prepends can leak into a publish. Check `sharezed diff` before `allow`.
