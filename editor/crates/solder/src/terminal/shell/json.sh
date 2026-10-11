# Bytewise escaping keeps terminal control characters out of the OSC payload.
# Both callers bound the input before this loop; no process is spawned.
_solder_json() {
    local LC_ALL=C _solder_s=$1 _solder_i _solder_ch _solder_n
    _solder_json_result=''
    for ((_solder_i=0; _solder_i<${#_solder_s}; _solder_i++)); do
        _solder_ch=${_solder_s:_solder_i:1}
        case $_solder_ch in
            '"') _solder_json_result+='\"' ;;
            '\') _solder_json_result+='\\' ;;
            # VTE limits the number of semicolon-separated OSC parameters.
            ';') _solder_json_result+='\u003b' ;;
            *)
                builtin printf -v _solder_n '%d' "'$_solder_ch"
                # Bash 3's printf treats non-ASCII bytes as signed chars.
                _solder_n=$(( _solder_n & 255 ))
                if (( _solder_n < 32 || _solder_n == 127 )); then
                    builtin printf -v _solder_ch '\\u%04x' "$_solder_n"
                fi
                _solder_json_result+=$_solder_ch
                ;;
        esac
    done
}

_solder_emit() {
    local LC_ALL=C _solder_command=$1 _solder_cwd=$2 _solder_status=$3
    local _solder_json_result _solder_command_json _solder_payload
    [[ -n $_solder_command && ${#_solder_command} -le 8192 && ${#_solder_cwd} -le 4096 ]] || return 0
    _solder_json "$_solder_command"
    _solder_command_json=$_solder_json_result
    _solder_json "$_solder_cwd"
    _solder_payload='{"command":"'$_solder_command_json'","cwd":"'$_solder_json_result'","status":'$_solder_status'}'
    [[ ${#_solder_payload} -le 16384 ]] || return 0
    builtin printf '\033]0;solder-command;%s;%s\007' "$_solder_token" "$_solder_payload"
}
