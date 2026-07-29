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
  local name attrs k
  local -a v
  for name attrs in ${(kv)parameters}; do
    (( ${_sz_deny[(Ie)$name]} )) && continue
    [[ $name == (SHAREZED_*|_sharezed_*|_sz_*|SZ_*) ]] && continue
    # Completion-system state is an explicit non-goal (§3), and it is most of
    # what a compinit'd shell holds: 1498 of 1511 functions on a real zshrc.
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
    # `_*` is zsh's convention for completion functions — out of scope (§3).
    [[ $name == _* ]] && continue
    # An autoloadable function is recorded by *presence*, never by body:
    # whether it has been called yet is a lazy-loading detail, and letting it
    # into the state makes every "did anything call zmv this run" a phantom diff.
    if _sz_is_autoload $name; then
      print -rN -- autoload "$name" "" 0
    else
      print -rN -- func "$name" "" 1 "$functions[$name]"
    fi
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
