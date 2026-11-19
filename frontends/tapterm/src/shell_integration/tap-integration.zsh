# Shell integration for zsh. Reports the working directory to tap
# (OSC 7) on every prompt and directory change, which powers the title
# path variables and opening new tabs in the current directory, and
# marks prompts and commands with OSC 133 so tap can tell prompt,
# command and output apart and announce a long command when it ends.
#
# Tap loads this file from the ZDOTDIR it points zsh at, before any of
# the user's rc files; running it twice is a no-op. A shell tap did not
# spawn (`exec zsh`, tmux, `sudo -E zsh`) can source it from
# `$TAP_SHELL_INTEGRATION/zsh/tap-integration.zsh`. Everything lives in
# functions so `emulate -L` only changes options locally, and builtins
# are spelled `builtin` so user functions of the same name are never
# called by accident.
if [[ -n "${_tap_state+x}" ]]; then
    return 0
fi

# Control bytes in a directory or host name could end the OSC early
# and start a sequence of their own, so they are dropped. `print -r`
# keeps backslashes literal; the frame is real ESC/BEL.
_tap_report_pwd() {
    builtin emulate -L zsh -o no_aliases
    builtin print -rn -- \
        $'\e]7;kitty-shell-cwd://'"${HOST//[[:cntrl:]]}${PWD//[[:cntrl:]]}"$'\a'
}

# Takes tap's marks back out of PS1 and PS2. The clean copy is used
# when a prompt is exactly as tap left it; when something edited the
# marked prompt in place, the marks are removed by text instead and
# the edit stays. (Comparisons and substitutions are literal:
# GLOB_SUBST is off here.)
_tap_unmark_prompts() {
    builtin emulate -L zsh -o no_warn_create_global -o no_aliases
    [[ -n ${_tap_marked_ps1+x} ]] || builtin return 0
    if [[ $PS1 == $_tap_marked_ps1 ]]; then
        PS1=$_tap_clean_ps1
    else
        PS1=${${${PS1//$_tap_mark_a}//$_tap_mark_b}//$_tap_mark_cont}
    fi
    if [[ $PS2 == $_tap_marked_ps2 ]]; then
        PS2=$_tap_clean_ps2
    else
        PS2=${${PS2//$_tap_mark_b}//$_tap_mark_cont}
    fi
}

_tap_setup() {
    builtin emulate -L zsh -o no_warn_create_global -o no_aliases
    [[ -o interactive ]] || builtin return 0

    # 0: no OSC 133 mark written yet.
    # 1: a `C` has been written and its `D` is still owed.
    # 2: the last `C` was closed.
    builtin typeset -gi _tap_state=0
    builtin typeset -g _tap_mark_a=$'%{\e]133;A\a%}'
    builtin typeset -g _tap_mark_b=$'%{\e]133;B\a%}'
    builtin typeset -g _tap_mark_cont=$'%{\e]133;P;k=s\a%}'

    # Known from the start, before any prompt: an rc file may run a
    # long program first, or drop the hook arrays altogether. The
    # chpwd report has no ordering needs, so it is installed now too:
    # `cd x && make` changes directory without a prompt in between.
    _tap_report_pwd
    builtin typeset -ag chpwd_functions precmd_functions
    chpwd_functions+=(_tap_report_pwd)

    # The prompt hooks are installed from the first precmd, once every
    # rc file has run, so they can sit around the user's own hooks: one
    # before them all, one after.
    precmd_functions+=(_tap_install_hooks)
}

_tap_install_hooks() {
    builtin emulate -L zsh -o no_warn_create_global -o no_aliases

    # First before each prompt, ahead of every other hook: close the
    # command with its status, before anything a hook prints, and take
    # last cycle's marks out of the prompts, so no hook ever sees them.
    # Enter on an empty line runs no preexec, so without this a hook
    # that edits PS1 in place (a venv prefix, say) would wrap the
    # marks and the prompt would grow at every empty line.
    _tap_precmd_first() {
        builtin local -i last_status=$?
        builtin emulate -L zsh -o no_warn_create_global -o no_aliases

        # A precmd run by a zle widget (some plugins redraw the prompt
        # that way) is not the end of a command, so no `D` then.
        if ! builtin zle; then
            if (( _tap_state == 1 )); then
                builtin print -n '\e]133;D;'$last_status'\a'
                (( _tap_state = 2 ))
            elif (( _tap_state == 2 )); then
                # Enter on an empty line runs no preexec: close
                # whatever may be open, with no status to give.
                builtin print -n '\e]133;D\a'
            fi
        fi

        _tap_unmark_prompts
    }

    # Last before each prompt, after the theme set PS1: mark it.
    _tap_precmd() {
        builtin emulate -L zsh -o no_warn_create_global -o no_aliases

        # Normally already clean; this covers the first hook having
        # been dropped from the list, which also puts it back.
        _tap_unmark_prompts
        if [[ ${precmd_functions[1]} != _tap_precmd_first ]]; then
            precmd_functions=(_tap_precmd_first ${precmd_functions:#_tap_precmd_first})
        fi

        if [[ -o prompt_percent ]]; then
            if [[ ${precmd_functions[-1]} == _tap_precmd ]]; then
                # Marks inside PS1/PS2 are re-emitted whenever zsh
                # redraws the prompt (reset-prompt, SIGWINCH), which a
                # bare print cannot do. Whatever PS1 is now, a theme's
                # own or last cycle's, is the clean copy to mark.
                _tap_clean_ps1=$PS1
                _tap_clean_ps2=$PS2

                # zsh drops a lone `%` at the end of a prompt, but with
                # a mark appended it would pair with the `{` into a
                # `%{` escape instead. Drop it here so the prompt looks
                # as it does unmarked.
                builtin local tail=${PS1##*[^%]}
                (( ${#tail} % 2 )) && PS1=${PS1[1,-2]}
                tail=${PS2##*[^%]}
                (( ${#tail} % 2 )) && PS2=${PS2[1,-2]}

                # Each further line of a multi-line prompt is a
                # continuation; so is every PS2 line.
                PS1=${PS1//$'\n'/$'\n'${_tap_mark_cont}}
                PS1=${_tap_mark_a}${PS1}${_tap_mark_b}
                PS2=${_tap_mark_cont}${PS2}${_tap_mark_b}
                _tap_marked_ps1=$PS1
                _tap_marked_ps2=$PS2
                (( _tap_state = 2 ))
            else
                # Another hook took the last slot: a PS1 edit may not
                # stick, so print the mark instead (unless from zle,
                # where the cursor is mid-line) and move to the end
                # for next time.
                precmd_functions=(${precmd_functions:#_tap_precmd} _tap_precmd)
                if ! builtin zle; then
                    builtin print -n '\e]133;A\a'
                    (( _tap_state = 2 ))
                fi
            fi
        elif ! builtin zle; then
            # Without prompt_percent PS1 cannot carry `%{ %}` escapes.
            builtin print -n '\e]133;A\a'
            (( _tap_state = 2 ))
        fi

        _tap_report_pwd
    }

    # The command line is complete and about to run. First among the
    # preexec hooks, so what the others print is output, after `C`,
    # and they see the clean prompts.
    _tap_preexec() {
        builtin emulate -L zsh -o no_warn_create_global -o no_aliases
        _tap_unmark_prompts
        builtin print -n '\e]133;C\a'
        (( _tap_state = 1 ))
    }

    builtin typeset -ag preexec_functions
    preexec_functions=(_tap_preexec ${preexec_functions:#_tap_preexec})
    builtin local -i was_last=0
    [[ ${precmd_functions[-1]} == _tap_install_hooks ]] && was_last=1
    precmd_functions=(
        _tap_precmd_first
        ${precmd_functions:#_tap_install_hooks}
        _tap_precmd
    )
    builtin unfunction _tap_install_hooks

    # This first precmd is already under way without the hooks. zsh
    # runs the hooks that follow this one from its copy of the list,
    # so when this was not the last one a theme's precmd still has to
    # set PS1: marking it now would be undone and the wrong PS1 kept
    # as the clean copy. The mark is printed bare instead, and PS1
    # carries it from the next prompt on.
    if (( was_last )); then
        _tap_precmd
    else
        builtin print -n '\e]133;A\a'
        (( _tap_state = 2 ))
        _tap_report_pwd
    fi
}

_tap_setup
builtin unfunction _tap_setup
