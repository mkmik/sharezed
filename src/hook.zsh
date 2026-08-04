# sharezed shell integration (PRD §7.5). Printed by `sharezed hook zsh`.
# Steady state is one open+read of an ~8-byte file and zero forks.

zmodload zsh/parameter

typeset -g SHAREZED_BIN=@BIN@
typeset -g SHAREZED_CHANNEL=@CHANNEL@
typeset -gx SHAREZED_HEAD=@HEAD@
typeset -gx SHAREZED_CONFLICTS=

# Appended to RPROMPT while a reload is pending, unless SHAREZED_NO_NOTIFY.
# The leading space is part of it so stripping puts your prompt back exactly.
typeset -g _sharezed_segment=' %F{yellow}↻ sharezed reload%f'

# A fresh shell has just run the bootstrap, so it already *is* the desired
# state — start at head, not at 0. Exported so `sharezed status` can read it.
# `2>/dev/null` cannot silence this: a failed *redirection* is reported by the
# shell on its own stderr, before the redirect it would have been muted by.
typeset -gx SHAREZED_CURSOR=0
[[ -r $SHAREZED_HEAD ]] && read -r SHAREZED_CURSOR < $SHAREZED_HEAD

# Merge base for a list param the log has never carried before (§8.9): what
# this shell had when the hook was installed.
typeset -ga SHAREZED_PATH0=( "$path[@]" )

# --- three-way merge support: is this key still what sharezed installed? -----

_sharezed_cur() {   # kind name -> reply=(value...); returns 1 if the key is absent
  local k=$1 n=$2 x
  reply=()
  case $k in
    scalar) (( ${+parameters[$n]} )) || return 1; reply=( "${(P)n}" ) ;;
    array)  (( ${+parameters[$n]} )) || return 1; reply=( "${(@P)n}" ) ;;
    assoc)  (( ${+parameters[$n]} )) || return 1
            local -A a=( "${(@kvP)n}" )
            for x in ${(ko)a}; do reply+=( "$x" "$a[$x]" ); done ;;
    func)   (( ${+functions[$n]} ))  || return 1; reply=( "$functions[$n]" ) ;;
    # Presence only: a stub and a loaded body are the same desired state.
    autoload) (( ${+functions[$n]} )) || return 1 ;;
    alias)  (( ${+aliases[$n]} ))    || return 1; reply=( "$aliases[$n]" ) ;;
    galias) (( ${+galiases[$n]} ))   || return 1; reply=( "$galiases[$n]" ) ;;
    salias) (( ${+saliases[$n]} ))   || return 1; reply=( "$saliases[$n]" ) ;;
  esac
  return 0
}

_sharezed_eq() {    # kind name expected... -> 0 when the live value still matches
  local -a reply
  _sharezed_cur $1 $2 || return 1
  shift 2
  (( $#reply == $# )) || return 1
  local e i=1
  for e; do
    [[ $reply[i] == "$e" ]] || return 1
    (( i++ ))
  done
  return 0
}

_sharezed_absent() {
  local -a reply
  if _sharezed_cur $1 $2; then return 1; else return 0; fi
}

_sharezed_conflict() {
  SHAREZED_CONFLICTS="$SHAREZED_CONFLICTS${SHAREZED_CONFLICTS:+:}$1"
}

# --- apply ------------------------------------------------------------------

_sharezed_apply() {
  local out n
  local -a v
  # Ordered-list params go to the tool as `ours` for the §8 merge; everything
  # else is guarded inline, so this is all the state that has to cross over.
  out=$(
    {
      for n in path fpath manpath cdpath; do
        (( ${+parameters[$n]} )) || continue
        v=( "${(@P)n}" )
        print -rN -- param $n "$parameters[$n]" $#v "$v[@]"
      done
      print -rN -- param '@base:path' array $#SHAREZED_PATH0 "$SHAREZED_PATH0[@]"
    } | $SHAREZED_BIN export zsh --channel $SHAREZED_CHANNEL --cursor $SHAREZED_CURSOR
  ) || return 1
  eval "$out"
}

_sharezed_precmd() {
  [[ -n $SHAREZED_DISABLE ]] && return 0
  # A moved or uninstalled binary must go quiet, not report an exec failure on
  # every prompt — which is what a default-on notify would otherwise do.
  [[ -x $SHAREZED_BIN ]] || return 0
  # Opt-in: publish your own config changes without typing `reload`. Costs a
  # fork and ~6ms per prompt, which is invisible — the reason it is off by
  # default is that it makes pressing enter a publish action, so a half-saved
  # zshrc reaches every shell at whatever moment you next hit a prompt.
  [[ -n $SHAREZED_AUTORELOAD ]] &&
    $SHAREZED_BIN reload --channel $SHAREZED_CHANNEL --silent
  # On by default: forgetting to reload is the failure mode this exists for.
  # Same fork as autoreload, but it only looks — the human still decides when
  # to publish. Strip first, then re-add: idempotent across prompts, and it
  # picks up an RPROMPT your config sets *after* the hook line without saving
  # a copy. No promptsubst needed — precmd runs before the prompt is rendered.
  #
  # --check is a fingerprint comparison, so most of what it flags publishes
  # nothing at all (a rewritten ~/.zcompdump is the everyday one). Settling
  # that unattended costs one capture — the same fork autoreload pays, and only
  # while something is dirty — and never publishes: a delta a human should look
  # at leaves the nag exactly where --check put it. The tool remembers such a
  # delta, so the retry on the next prompt is a fingerprint check again, not a
  # second capture. SHAREZED_NO_SETTLE keeps the nag and the fork out of it.
  if [[ -z $SHAREZED_NO_NOTIFY ]]; then
    RPROMPT=${RPROMPT%"$_sharezed_segment"}
    if ! $SHAREZED_BIN reload --channel $SHAREZED_CHANNEL --check --silent &&
       { [[ -n $SHAREZED_NO_SETTLE ]] ||
         ! $SHAREZED_BIN reload --channel $SHAREZED_CHANNEL --if-noop --silent }
    then
      RPROMPT+=$_sharezed_segment
    fi
  fi
  [[ -r $SHAREZED_HEAD ]] || return 0
  local head
  read -r head < $SHAREZED_HEAD || return 0
  [[ $head == $SHAREZED_CURSOR ]] && return 0
  if ! _sharezed_apply; then
    # ponytail: quarantine after one failure, not N. A poison entry costs a
    # message, not a dead prompt (§7.8.3).
    typeset -gx SHAREZED_DISABLE=1
    print -u2 "sharezed: apply failed at seq $head — disabled in this shell (unset SHAREZED_DISABLE to retry)"
  fi
}

autoload -Uz add-zsh-hook
add-zsh-hook precmd _sharezed_precmd
