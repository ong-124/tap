# Sourced by zsh because tap pointed ZDOTDIR here. Restores the user's
# ZDOTDIR, chains their real .zshenv, then loads the integration for
# interactive shells, so their own configuration always runs first.
if [[ -n "${TAP_ZSH_ZDOTDIR+x}" ]]; then
    builtin export ZDOTDIR="$TAP_ZSH_ZDOTDIR"
    builtin unset TAP_ZSH_ZDOTDIR
else
    builtin unset ZDOTDIR
fi

builtin typeset _tap_user_zshenv="${ZDOTDIR:-$HOME}/.zshenv"
if [[ -r "$_tap_user_zshenv" && ! -d "$_tap_user_zshenv" ]]; then
    builtin source -- "$_tap_user_zshenv"
fi
builtin unset _tap_user_zshenv

if [[ -o interactive ]]; then
    builtin typeset _tap_integration_file="${${(%):-%x}:A:h}/tap-integration.zsh"
    if [[ -r "$_tap_integration_file" ]]; then
        builtin source -- "$_tap_integration_file"
    fi
    builtin unset _tap_integration_file
fi
