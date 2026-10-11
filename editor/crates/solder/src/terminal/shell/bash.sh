[[ $- == *i* && -z ${_solder_installed-} ]] || return 0
_solder_installed=1
_solder_token=$SOLDER_SHELL_TOKEN
builtin source "$SOLDER_SHELL_DIR/json.sh"
unset SOLDER_SHELL_TOKEN SOLDER_SHELL_DIR SOLDER_SHELL_HOME

# Bash 3 has only a string PROMPT_COMMAND; newer Bash also accepts an array.
_solder_user_prompt=()
if [[ $(builtin declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a'* ]]; then
    _solder_user_prompt=("${PROMPT_COMMAND[@]}")
elif [[ -n ${PROMPT_COMMAND-} ]]; then
    _solder_user_prompt=("$PROMPT_COMMAND")
fi

_solder_history() {
    local HISTTIMEFORMAT='' _solder_line
    _solder_history_number=0
    _solder_history_command=''
    _solder_line=$(builtin history 1)
    if [[ $_solder_line =~ ^[[:space:]]*([0-9]+)[[:space:]]+(.*)$ ]]; then
        _solder_history_number=${BASH_REMATCH[1]}
        _solder_history_command=${BASH_REMATCH[2]}
    fi
}

_solder_last_history=0
_solder_prompt_ready=0
# The user's hooks run first, with the original $? and PIPESTATUS. Merely
# restoring a scalar status after bookkeeping would destroy pipeline data.
_solder_prompt() {
    for _solder_hook in "${_solder_user_prompt[@]}"; do
        builtin eval -- "$_solder_hook"
    done
    _solder_after_prompt "$1" "$?"
}

_solder_after_prompt() {
    local _solder_status=$1 _solder_hook_status=$2
    local _solder_history_number _solder_history_command
    [[ ${#_solder_user_prompt[@]} -gt 0 ]] || _solder_hook_status=$_solder_status
    _solder_history
    # Importing history in a user hook can add someone else's commands.
    # Skip a jump rather than assigning our status to an imported entry.
    if [[ $_solder_prompt_ready == 1 && -o history ]] &&
        (( _solder_history_number == _solder_last_history + 1 )); then
        _solder_emit "$_solder_history_command" "$_solder_prompt_cwd" "$_solder_status"
    fi
    _solder_last_history=$_solder_history_number
    _solder_prompt_cwd=$PWD
    _solder_prompt_ready=1
    return "$_solder_hook_status"
}
PROMPT_COMMAND='_solder_prompt "$?"'
