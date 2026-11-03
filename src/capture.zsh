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

# "Skip special params" is too aggressive — PATH is special (§5.4a), and so is
# every prompt. The rest of the writable specials are the shell describing its
# surroundings (TERM, COLUMNS, HOME, UID, SHLVL) and syncing those across
# terminals would be actively wrong, so this stays an allowlist.
#
# Only the PS* spelling of each prompt is listed: PROMPT/prompt/PROMPT2… are
# the same parameters under a second name, and both names in the table would
# publish one edit twice. RPS1/RPROMPT are the exception — neither exists until
# something assigns one, so the name your config used is the only one to catch.
_sz_allowed_special() {
  [[ $1 == (PATH|path|FPATH|fpath|MANPATH|manpath|CDPATH|cdpath) ||
     $1 == (PS[1-4]|SPROMPT|RPS[12]|RPROMPT|RPROMPT2) ||
     $1 == (HISTSIZE|SAVEHIST|WORDCHARS) ]]
}

# Autoloadable iff it is still a stub, or it was loaded from a file in $fpath
# named exactly after it. The second half matters: $functions_source[compdef]
# is `…/compinit`, and `autoload -Uz compdef` would stub out to nothing.
_sz_is_autoload() {
  [[ $functions[$1] == "builtin autoload -X"* ]] && return 0
  local src=${functions_source[$1]:-}
  [[ -n $src && ${src:t} == $1 ]] && (( ${fpath[(Ie)${src:h}]} ))
}

# Options the shell sets for itself (how it was started, what the harness
# traces with) or that would make the apply function's own setopts local.
typeset -ga _sz_opt_deny=(
  interactive login shinstdin singlecommand privileged restricted monitor zle
  rcs globalrcs xtrace sourcetrace localoptions localtraps localloops
  localpatterns
)

# byte -> REPLY, bindkey's own spelling of that one key: `\M-` for the high
# bit, `^` for a control character, and a backslash before `\` and `^`.
_sz_kn() {
  local -i c=$1
  REPLY=
  (( c > 127 )) && { REPLY='\M-'; (( c -= 128 )) }
  (( c < 32 || c == 127 )) && { REPLY+='^'; (( c ^= 64 )) }
  (( c == 92 || c == 94 )) && REPLY+='\'
  REPLY+=${(#)c}
}

# _sz_kn backwards: one-key spelling -> REPLY, its byte.
_sz_kb() {
  local n=$1
  local -i b=0
  [[ $n == '\M-'* ]] && { b=128; n=${n#'\M-'} }
  [[ $n == '^'?* ]] && { (( b |= 64 )); n=${n#'^'} }
  [[ $n == '\'? ]] && n=${n#'\'}
  REPLY=$(( (b & 128) | ((#n) ^ (b & 64)) ))
}

# keymap -> reply=( key value ... ) for every key it binds. Ranges expand one
# key at a time: the listing folds neighbours bound alike into one
# (`bindkey a x; bindkey b x` lists as `"a"-"b" x`), so a range is how a map
# prints, never what was bound. The value is the listing's own shell-quoted
# words — a widget, or `-s string` — and `undefined-key` is no binding at all.
_sz_bkmap() {
  setopt localoptions extendedglob
  # REPLY is the helpers' scratch: local, so a bootstrap's `typeset -i REPLY`
  # neither breaks it nor sees it change.
  local line val REPLY
  local -a w out match mbegin mend
  local -i i lo hi
  for line in ${(f)"$(bindkey -L -M $1)"}; do
    w=( ${(z)line} )
    [[ $w[-1] == undefined-key ]] && continue
    val=$w[-1]
    (( ${${w[2,-3]}[(Ie)-s]} )) && val="-s $val"
    if (( ${${w[2,-3]}[(Ie)-R]} )); then
      [[ $w[-2] == (#b)\"(*)\"-\"(*)\" ]] || continue
      _sz_kb "${(Q):-\"$match[1]\"}"; lo=$REPLY
      _sz_kb "${(Q):-\"$match[2]\"}"; hi=$REPLY
      for (( i = lo; i <= hi; i++ )); do
        _sz_kn $i
        out+=( "$REPLY" "$val" )
      done
    else
      out+=( "${(Q)w[-2]}" "$val" )
    fi
  done
  reply=( "$out[@]" )
}

_sz_dump() {
  # Capture before you perturb (§5.4b): `emulate -L` below resets $options.
  # Every local is `_sz_`-named: a plain `local line` would hide your global
  # `line` from the $parameters walk below.
  local -A _sz_o=( "${(@kv)options}" )
  emulate -L zsh
  local _sz_n _sz_t _sz_k _sz_g _sz_l
  local -a _sz_v _sz_zl _sz_h
  # ZLE loads on first use, so a bootstrap that never touched it has nothing
  # here — and loading it to look would pick `main` early (see _sz_main0).
  # The functions `zle -N` widgets call are carried even when `_`-named.
  local -A _sz_wf
  if zmodload -e zsh/zle; then
    for _sz_l in ${(f)"$(zle -lL)"}; do
      _sz_v=( "${(@Q)${(z)_sz_l}}" )
      # `zle -N -- -w f` lists with its `--`.
      [[ $_sz_v[3] == -- ]] && _sz_v[3]=()
      _sz_zl+=( "${(j: :)${(@q)_sz_v}}" )
      if [[ $_sz_v[2] == -N ]]; then
        _sz_wf[${_sz_v[4]:-$_sz_v[3]}]=1
      # A completion widget's function is yours unless zsh ships it: compinit's
      # `_main_complete` lives in a stock $fpath directory, `_my_complete` not.
      elif [[ $_sz_v[2] == -C ]]; then
        _sz_h=( $^_sz_fpath0/$_sz_v[-1](N) )
        (( $#_sz_h )) || _sz_wf[$_sz_v[-1]]=1
      fi
    done
  fi
  for _sz_n in ${(ko)_sz_o}; do
    (( ${_sz_opt_deny[(Ie)$_sz_n]} )) && continue
    print -rN -- option "$_sz_n" "" 1 "$_sz_o[$_sz_n]"
  done
  for _sz_n _sz_t in ${(kv)parameters}; do
    (( ${_sz_deny[(Ie)$_sz_n]} )) && continue
    # Reading either autoloads ZLE, which must wait for the bootstrap (see
    # _sz_main0) — and both are readonly once loaded.
    [[ $_sz_n == (widgets|keymaps) && $_sz_t == undefined ]] && continue
    # Only the hook's *own* per-shell state is excluded (§5.3) — a shared
    # cursor or head is nonsense. The rest of the namespace is configuration
    # you wrote (SHAREZED_NOTIFY, SHAREZED_AUTORELOAD, …) and propagates like
    # anything else. SHAREZED_DISABLE stays out on purpose: a synced kill
    # switch would disable every shell, leaving nothing able to apply its
    # removal (G7).
    (( ${_sz_hook_state[(Ie)$_sz_n]} )) && continue
    [[ $_sz_n == (_sz_*|SZ_*) ]] && continue
    # Completion-system state is an explicit non-goal (§3), and it is most of
    # what a compinit'd shell holds: 1498 of 1511 functions on a real zshrc.
    # `_comps` goes out whole here; the handful of entries worth syncing are
    # emitted one by one as `compdef` records below.
    [[ $_sz_n == (_comp*|_patcomps|_postpatcomps|_services|_lastcomp|comppostfuncs|compprefuncs) ]] && continue
    # Hook arrays name functions a receiving shell may not have — and could
    # drop sharezed's own precmd. Per-shell wiring, never synced (§5.2, G7).
    [[ $_sz_n == *_functions ]] && continue
    [[ $_sz_t == *readonly* || $_sz_t == *local* ]] && continue
    [[ $_sz_t == *special* ]] && ! _sz_allowed_special $_sz_n && continue
    # Tied pairs: carry the array side, suppress the scalar twin (§5.4c).
    [[ $_sz_t == *tied* && $_sz_t != array* ]] && continue
    case $_sz_t in
      association*)
        local -A _sz_a=( "${(@kvP)_sz_n}" )
        _sz_v=()
        for _sz_k in ${(ko)_sz_a}; do _sz_v+=( "$_sz_k" "$_sz_a[$_sz_k]" ); done ;;
      array*) _sz_v=( "${(@P)_sz_n}" ) ;;
      *)      _sz_v=( "${(P)_sz_n}" ) ;;
    esac
    # Loading ZLE creates it, so S₀ never has it: only a changed one is yours.
    [[ $_sz_n == zle_bracketed_paste && ${(pj:\0:)_sz_v} == "$_sz_zbp0" ]] && continue
    print -rN -- param "$_sz_n" "$_sz_t" $#_sz_v "$_sz_v[@]"
  done

  for _sz_n in ${(k)functions}; do
    # An autoloadable function is recorded by *presence*, never by body:
    # whether it has been called yet is a lazy-loading detail, and letting it
    # into the state makes every "did anything call zmv this run" a phantom diff.
    if _sz_is_autoload $_sz_n; then
      # `_*` is zsh's convention for completion functions, and compinit stubs
      # out ~1500 of them from $fpath — the completion system itself, an
      # explicit non-goal (§3). A `_*` function with a *body* is not one of
      # those: it is a function your config wrote (`ccwt init zsh` defines
      # `_ccwt`, `fp completions zsh` defines `_fp`), so it syncs like any
      # other. SHAREZED_IGNORE is the valve if one of them is too fat. So is
      # one a widget calls: compinit never makes a `zle -N` widget.
      [[ $_sz_n == _* ]] && (( ! ${+_sz_wf[$_sz_n]} )) && continue
      print -rN -- autoload "$_sz_n" "" 0
    else
      print -rN -- func "$_sz_n" "" 1 "$functions[$_sz_n]"
    fi
  done
  # The `compdef` calls your config made. `_comps` itself never syncs — compinit
  # builds ~1900 entries from the `#compdef` tags in $fpath and rebuilds them in
  # every shell — but the entries pointing at a function *this dump carries* are
  # yours, and there is exactly one kind of function that qualifies: one your
  # config defined. On a stock compinit that filter keeps 0 of 1874. Without
  # them a new completion syncs its function and nothing binds it to a command.
  # An entry is a command line, not always a bare name: bashcompinit's
  # `complete` stores `_bash_complete -o default -F _foo`. Judge it by its
  # first word, publish it whole.
  for _sz_n in ${(k)_comps}; do
    _sz_g=${_comps[$_sz_n]%% *}
    (( ${+functions[$_sz_g]} )) || continue
    _sz_is_autoload $_sz_g && continue
    print -rN -- compdef "$_sz_n" "" 1 "$_comps[$_sz_n]"
  done
  for _sz_n _sz_t in ${(kv)aliases};  do print -rN -- alias  "$_sz_n" "" 1 "$_sz_t"; done
  for _sz_n _sz_t in ${(kv)galiases}; do print -rN -- galias "$_sz_n" "" 1 "$_sz_t"; done
  for _sz_n _sz_t in ${(kv)saliases}; do print -rN -- salias "$_sz_n" "" 1 "$_sz_t"; done

  # The keymap `main` links to, asked of a subshell: one that has loaded ZLE
  # answers as this shell will, and one that hasn't loads it there, picking
  # from $VISUAL as this shell's first prompt will. Linked to one of the stock
  # keymaps, or nothing to say: deleting the one it pointed at leaves `main` a
  # keymap of its own.
  local _sz_m=${${(z)"$(bindkey -lL main)"}[3]}
  [[ $_sz_m != $_sz_main0 ]] && (( ${_sz_maps[(Ie)$_sz_m]} )) &&
    print -rN -- bindkey main "$_sz_main0" 1 "$_sz_m"

  # Widgets: what `zle -N` and `zle -C` made. compinit's `-C` ones ride along
  # like any other: without them, dropping your `zle -N expand-or-complete
  # mine` would read as a removal and put back the raw builtin, not completion.
  zmodload -e zsh/zle || return 0
  zmodload zsh/zleparameter
  local -A _sz_cur _sz_ln
  local -a reply keys
  local m key cur def
  for _sz_l in $_sz_zl; do
    _sz_v=( "${(@Q)${(z)_sz_l}}" )
    _sz_ln[$_sz_v[3]]=1
    print -rN -- zle "$_sz_v[3]" "" $(( $#_sz_v - 2 )) "$_sz_v[2]" "${(@)_sz_v[4,-1]}"
  done
  # An alias of a builtin, which `zle -lL` can't show; one of a user widget
  # lists above as `-N`.
  for _sz_k cur in ${(kv)_sz_zla}; do
    (( ${+_sz_ln[$_sz_k]} || ! ${+widgets[$_sz_k]} )) && continue
    print -rN -- zle "$_sz_k" "" 2 -A "$cur"
  done
  # Bindings, as the difference from the clean-shell baseline taken before the
  # bootstrap ran. The default rides along as attrs: dropping a binding from
  # your zshrc puts back what it replaced, which the value alone can't say.
  # A keymap the bootstrap deleted (`bindkey -D emacs`) lists as empty, which
  # would publish every key in it as unbound. Deleting one is not carried.
  local -a live=( ${(f)"$(bindkey -l)"} )
  for m in $_sz_maps; do
    (( ${live[(Ie)$m]} )) || continue
    _sz_bkmap $m; _sz_cur=( "$reply[@]" )
    keys=( ${(k)_sz_cur} ${${(M)${(k)_sz_def0}:#$m *}#$m } )
    for key in ${(u)keys}; do
      cur=${_sz_cur[$key]-}
      def=${_sz_def0[$m $key]-}
      [[ $cur == "$def" ]] && continue
      _sz_v=()
      [[ -n $cur ]] && _sz_v=( "${(@Q)${(z)cur}}" )
      # Unquoted for the payload: `widget`, or `-s string` (vicmd's `guu`).
      print -rN -- bindkey "$m $key" "${(j: :)${(@Q)${(z)def}}}" $#_sz_v "$_sz_v[@]"
    done
  done
}

# Keymaps as a clean shell has them, read before the bootstrap can touch them:
# into variables, not keymap copies `bindkey -d` would delete, and in
# subshells. ZLE loads on first use and links `main` to viins if $VISUAL or
# $EDITOR says vi as it does, so loading it out here would choose emacs before
# your zshrc's `export VISUAL=vim` — which a real shell's prompt sees.
# `.safe` is read-only and `main` is a link to one of the others.
# ponytail: keymaps the bootstrap creates (`bindkey -N mine`) are not carried.
typeset -ga _sz_fpath0=( $fpath )
typeset -ga _sz_maps=( ${${(f)"$(bindkey -l)"}:#(.safe|main)} )
typeset -g _sz_main0=${${(z)"$(bindkey -lL main)"}[3]}
typeset -g _sz_zbp0="$(zmodload zsh/zle; print -rn -- ${(pj:\0:)zle_bracketed_paste})"
_sz_snap() {
  local m k v
  local -a reply
  for m in $_sz_maps; do
    _sz_bkmap $m
    for k v in "$reply[@]"; do print -rN -- "$m $k" "$v"; done
  done
}
typeset -gA _sz_def0=( ${(0)"$(_sz_snap)"} )

# Some params (LOGCHECK, WATCHFMT …) don't materialize in
# $parameters until something reads them — so dumping *is* what creates them.
# Warm up first, or they show up in S₁ only and look like the bootstrap's work.
_sz_dump >| /dev/null
# Split the trace into the files the bootstrap sourced and the externals it
# ran, re-emitting anything else as the genuine diagnostic it is. Done here
# rather than in the host because the shell is the only authority on which
# words are builtins — `[`, `which` and `command` all have /usr/bin twins.
# `zle -A .accept-line saved` leaves nothing `zle -lL` lists — an alias of a
# builtin is indistinguishable from the builtin — so the trace is the record,
# `builtin zle` included. _sz_zla maps each such alias to the builtin it stands
# for, resolved as the trace runs: `zle -A accept-line saved` saves whatever
# accept-line *was*, and a later `zle -N accept-line wrap` doesn't change that.
# ponytail: the trace can't tell a subshell's `( zle -A … )` from the shell's
# own, so one would win here. Thread $ZSH_SUBSHELL through PS4 if a real
# config ever aliases widgets inside a subshell.
typeset -gA _sz_zla _sz_zuser
_sz_zletrace() {
  local -a w=( "${(@Q)${(z)1}}" )
  [[ $w[1] == builtin ]] && w[1]=()
  (( $#w >= 3 )) || return 0
  local f=$w[2] src
  w=( "${(@)w[3,-1]}" )
  [[ $w[1] == -- ]] && w[1]=()
  case $f in
    -A)
      (( $#w == 2 )) || return 0
      src=$w[1]
      if (( ${+_sz_zla[$src]} )); then _sz_zla[$w[2]]=$_sz_zla[$src]
      elif [[ $src == .* ]]; then _sz_zla[$w[2]]=$src
      # An alias of a widget the bootstrap made lists in `zle -lL` as `-N`.
      elif (( ${+_sz_zuser[$src]} )); then unset "_sz_zla[$w[2]]"; _sz_zuser[$w[2]]=1
      else _sz_zla[$w[2]]=.$src
      fi ;;
    -[NC]) (( $#w )) && { unset "_sz_zla[$w[1]]"; _sz_zuser[$w[1]]=1 } ;;
    -D) for src in $w; do unset "_sz_zla[$src]" "_sz_zuser[$src]"; done ;;
  esac
}

_sz_untrace() {
  # The bootstrap's options are still in force: `nounset` must not reach here.
  emulate -L zsh
  setopt extendedglob
  local line w d
  local -A seen
  while IFS= read -r line; do
    if [[ $line == [+#]##*'> <sourcetrace>' ]]; then
      line=${line%'> <sourcetrace>'}
      print -r -- ${${line##[+#]##}%:*} >> $SZ_SRC
    elif [[ $line == [+#]##*'> '* ]]; then
      [[ $line == [+#]##*'> '(builtin )#zle\ * ]] && _sz_zletrace "${line#*'> '}"
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
