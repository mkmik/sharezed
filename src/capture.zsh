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

_sz_dump() {
  emulate -L zsh   # safe: nothing captured here is option-sensitive (§5.4b)
  local name attrs k
  local -a v
  for name attrs in ${(kv)parameters}; do
    (( ${_sz_deny[(Ie)$name]} )) && continue
    [[ $name == (SHAREZED_*|_sharezed_*|_sz_*|SZ_OUT*|SZ_BOOT) ]] && continue
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
    print -rN -- func "$name" "" 1 "$functions[$name]"
  done
  for name attrs in ${(kv)aliases};  do print -rN -- alias  "$name" "" 1 "$attrs"; done
  for name attrs in ${(kv)galiases}; do print -rN -- galias "$name" "" 1 "$attrs"; done
  for name attrs in ${(kv)saliases}; do print -rN -- salias "$name" "" 1 "$attrs"; done
}

# Some params (LOGCHECK, WATCHFMT, zle_bracketed_paste …) don't materialize in
# $parameters until something reads them — so dumping *is* what creates them.
# Warm up first, or they show up in S₁ only and look like the bootstrap's work.
_sz_dump >| /dev/null
_sz_dump >| $SZ_OUT0
[[ -n $SZ_BOOT ]] && source $SZ_BOOT
_sz_dump >| $SZ_OUT1
return 0
