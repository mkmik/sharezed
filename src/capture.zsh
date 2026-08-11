# Clean-room capture harness (PRD §7.1, Appendix A). Sourced by `zsh -f -i`.
# Reads SZ_OUT0 / SZ_OUT1 / SZ_BOOT from the environment; everything else it
# touches lives in the _sz_ namespace, which is excluded from capture.

zmodload zsh/parameter

typeset -ga _sz_deny=(
  PWD OLDPWD SHLVL _ RANDOM SECONDS EPOCHSECONDS LINENO HISTCMD
  funcstack functrace funcfiletrace ZSH_SUBSHELL TTY TTYIDLE
  ZSH_EVAL_CONTEXT status pipestatus ZSH_SCRIPT ZSH_ARGZERO
  ZSH_EXECUTION_STRING argv ARGC PPID COLUMNS LINES
  TRAPEXIT TRAPINT TRAPTERM TRAPHUP TRAPQUIT TRAPUSR1 TRAPUSR2
)

# Three namespaces. `_sharezed_*` is sharezed's own: synced, and theirs-wins
# at apply time, which is how a hook change reaches a running shell.
# `SHAREZED_*` is configuration you wrote: synced like any other variable.
# The list below is genuinely per-shell and never syncs — a cursor, a conflict
# list, the path this shell started with, the channel it subscribed to.
typeset -ga _sz_hook_state=(
  SHAREZED_BIN SHAREZED_CHANNEL SHAREZED_HEAD SHAREZED_CURSOR
  SHAREZED_CONFLICTS SHAREZED_PATH0 SHAREZED_DISABLE
)

# "Skip special params" is too aggressive — PATH is special (§5.4a).
_sz_allowed_special() {
  [[ $1 == (PATH|path|FPATH|fpath|MANPATH|manpath|CDPATH|cdpath) ]]
}

# Autoloadable iff it is still a stub, or it was loaded from a file in $fpath
# named exactly after it. The second half matters: $functions_source[compdef]
# is `…/compinit`, and `autoload -Uz compdef` would stub out to nothing.
_sz_is_autoload() {
  [[ $functions[$1] == "builtin autoload -X"* ]] && return 0
  local src=${functions_source[$1]:-}
  [[ -n $src && ${src:t} == $1 ]] && (( ${fpath[(Ie)${src:h}]} ))
}

_sz_dump() {
  emulate -L zsh   # safe: nothing captured here is option-sensitive (§5.4b)
  local name attrs k target
  local -a v
  for name attrs in ${(kv)parameters}; do
    (( ${_sz_deny[(Ie)$name]} )) && continue
    # Only the hook's *own* per-shell state is excluded (§5.3) — a shared
    # cursor or head is nonsense. The rest of the namespace is configuration
    # you wrote (SHAREZED_NOTIFY, SHAREZED_AUTORELOAD, …) and propagates like
    # anything else. SHAREZED_DISABLE stays out on purpose: a synced kill
    # switch would disable every shell, leaving nothing able to apply its
    # removal (G7).
    (( ${_sz_hook_state[(Ie)$name]} )) && continue
    [[ $name == (_sz_*|SZ_*) ]] && continue
    # Completion-system state is an explicit non-goal (§3), and it is most of
    # what a compinit'd shell holds: 1498 of 1511 functions on a real zshrc.
    # `_comps` goes out whole here; the handful of entries worth syncing are
    # emitted one by one as `compdef` records below.
    [[ $name == (_comp*|_patcomps|_postpatcomps|_services|_lastcomp|comppostfuncs|compprefuncs) ]] && continue
    # Hook arrays name functions a receiving shell may not have — and could
    # drop sharezed's own precmd. Per-shell wiring, never synced (§5.2, G7).
    [[ $name == *_functions ]] && continue
    [[ $attrs == *readonly* || $attrs == *local* ]] && continue
    [[ $attrs == *special* ]] && ! _sz_allowed_special $name && continue
    # Tied pairs: carry the array side, suppress the scalar twin (§5.4c).
    [[ $attrs == *tied* && $attrs != array* ]] && continue
    case $attrs in
      association*)
        local -A _sz_a=( "${(@kvP)name}" )
        v=()
        for k in ${(ko)_sz_a}; do v+=( "$k" "$_sz_a[$k]" ); done ;;
      array*) v=( "${(@P)name}" ) ;;
      *)      v=( "${(P)name}" ) ;;
    esac
    print -rN -- param "$name" "$attrs" $#v "$v[@]"
  done

  for name in ${(k)functions}; do
    # An autoloadable function is recorded by *presence*, never by body:
    # whether it has been called yet is a lazy-loading detail, and letting it
    # into the state makes every "did anything call zmv this run" a phantom diff.
    if _sz_is_autoload $name; then
      # `_*` is zsh's convention for completion functions, and compinit stubs
      # out ~1500 of them from $fpath — the completion system itself, an
      # explicit non-goal (§3). A `_*` function with a *body* is not one of
      # those: it is a function your config wrote (`ccwt init zsh` defines
      # `_ccwt`, `fp completions zsh` defines `_fp`), so it syncs like any
      # other. SHAREZED_IGNORE is the valve if one of them is too fat.
      [[ $name == _* ]] && continue
      print -rN -- autoload "$name" "" 0
    else
      print -rN -- func "$name" "" 1 "$functions[$name]"
    fi
  done
  # The `compdef` calls your config made. `_comps` itself never syncs — compinit
  # builds ~1900 entries from the `#compdef` tags in $fpath and rebuilds them in
  # every shell — but the entries pointing at a function *this dump carries* are
  # yours, and there is exactly one kind of function that qualifies: one your
  # config defined. On a stock compinit that filter keeps 0 of 1874. Without
  # them a new completion syncs its function and nothing binds it to a command.
  for name in ${(k)_comps}; do
    target=$_comps[$name]
    (( ${+functions[$target]} )) || continue
    _sz_is_autoload $target && continue
    print -rN -- compdef "$name" "" 1 "$target"
  done
  for name attrs in ${(kv)aliases};  do print -rN -- alias  "$name" "" 1 "$attrs"; done
  for name attrs in ${(kv)galiases}; do print -rN -- galias "$name" "" 1 "$attrs"; done
  for name attrs in ${(kv)saliases}; do print -rN -- salias "$name" "" 1 "$attrs"; done
}

# Some params (LOGCHECK, WATCHFMT, zle_bracketed_paste …) don't materialize in
# $parameters until something reads them — so dumping *is* what creates them.
# Warm up first, or they show up in S₁ only and look like the bootstrap's work.
_sz_dump >| /dev/null
# Split the trace into the files the bootstrap sourced and the externals it
# ran, re-emitting anything else as the genuine diagnostic it is. Done here
# rather than in the host because the shell is the only authority on which
# words are builtins — `[`, `which` and `command` all have /usr/bin twins.
_sz_untrace() {
  setopt localoptions extendedglob
  local line w d
  local -A seen
  while IFS= read -r line; do
    if [[ $line == [+#]##*'> <sourcetrace>' ]]; then
      line=${line%'> <sourcetrace>'}
      print -r -- ${${line##[+#]##}%:*} >> $SZ_SRC
    elif [[ $line == [+#]##*'> '* ]]; then
      w=${${line#*'> '}%% *}
      [[ -z $w || $w == *=* ]] && continue
      (( ${+seen[$w]} )) && continue
      seen[$w]=1
      (( ${+builtins[$w]} )) && continue
      if [[ $w == /* ]]; then
        [[ -x $w ]] && print -r -- $w >> $SZ_CMDS
      else
        for d in $path; do
          [[ -x $d/$w ]] && { print -r -- $d/$w >> $SZ_CMDS; break }
        done
      fi
    else
      print -ru2 -- $line
    fi
  done
}

# With no explicit bootstrap, run the startup sequence zsh itself would run.
# Verified byte-identical to `zsh -l -i` on a real config — and it is the only
# way to see ~/.zshenv, which is where `~/.cargo/bin` actually comes from.
typeset -ga _sz_startup=(
  /etc/zshenv  ${ZDOTDIR:-$HOME}/.zshenv
  /etc/zprofile ${ZDOTDIR:-$HOME}/.zprofile
  /etc/zshrc   ${ZDOTDIR:-$HOME}/.zshrc
  /etc/zlogin  ${ZDOTDIR:-$HOME}/.zlogin
)

_sz_dump >| $SZ_OUT0
# SOURCE_TRACE names every file the bootstrap loads; XTRACE names every command
# it runs — including inside `$(…)` and `<(…)`, which fork and so never reach
# the parent's command hash table. Both are out-of-scope as state (§3); they
# feed the trust gate and staleness reporting only.
: >| $SZ_SRC; : >| $SZ_CMDS
setopt sourcetrace xtrace
# unsetopt goes inside: outside the redirect it would trace itself onto the
# caller's stderr.
# A `{ }` block, never a function: inside a function `typeset -a foo=(…)` in a
# zshrc would declare a *local* and vanish with the frame (§5.4d).
{
  if [[ -n $SZ_BOOT ]]; then
    source $SZ_BOOT
  else
    for _sz_f in $_sz_startup; do
      [[ -r $_sz_f ]] && source $_sz_f
    done
  fi
  unsetopt xtrace sourcetrace
} 2>| $SZ_TRACE
_sz_untrace < $SZ_TRACE
_sz_dump >| $SZ_OUT1
return 0
