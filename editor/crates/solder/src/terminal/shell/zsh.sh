[[ -o interactive && -z ${_solder_installed-} ]] || return 0
typeset -g _solder_installed=1 _solder_token=$SOLDER_SHELL_TOKEN
builtin source "$SOLDER_SHELL_DIR/json.sh"
unset SOLDER_SHELL_TOKEN SOLDER_SHELL_DIR SOLDER_SHELL_ZDOTDIR SOLDER_SHELL_ZDOTDIR_SET

# Named hooks run before the arrays. Move their bodies into those arrays so the
# collector gets the command's status and cwd before a user hook changes either.
if (( $+functions[preexec] )); then
    functions[_solder_user_preexec]=$functions[preexec]
    preexec_functions=(_solder_user_preexec "${preexec_functions[@]}")
fi
if (( $+functions[precmd] )); then
    functions[_solder_user_precmd]=$functions[precmd]
    precmd_functions=(_solder_user_precmd "${precmd_functions[@]}")
fi
preexec() {
    local _solder_status=$?
    typeset -g _solder_command=$1 _solder_cwd=$PWD _solder_pending=1
    return "$_solder_status"
}
precmd() {
    local _solder_status=$?
    if [[ ${_solder_pending-} == 1 ]]; then
        _solder_emit "$_solder_command" "$_solder_cwd" "$_solder_status"
        _solder_pending=0
    fi
    return "$_solder_status"
}
