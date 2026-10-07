# bash completion for codecleanup
#
# Load it with:  source <(codecleanup --completions=bash)
# or install it: codecleanup --completions=bash > ~/.local/share/bash-completion/completions/codecleanup

_codecleanup() {
    local cur=${COMP_WORDS[COMP_CWORD]}
    local prev=${COMP_WORDS[COMP_CWORD-1]}
    # `=` is a word break in bash: `--in=sr` arrives as `--in`, `=`, `sr`.
    if [[ $cur == "=" ]]; then
        cur=""
    elif [[ $prev == "=" ]]; then
        prev=${COMP_WORDS[COMP_CWORD-2]}
    fi
    COMPREPLY=()

    local IFS=$'\n'
    case $prev in
        --in | --docs | --plugins)
            # Paths are completed like for any other command: relative,
            # absolute and `~/`, one path segment at a time.
            compopt -o filenames -o nospace 2>/dev/null
            local tilde=""
            if [[ $cur == "~/"* ]]; then
                tilde=1
                cur=$HOME/${cur#"~/"}
            fi
            if [[ $prev == --plugins ]]; then
                COMPREPLY=($(compgen -d -- "$cur"))
            else
                COMPREPLY=($(compgen -f -- "$cur"))
            fi
            if [[ -n $tilde ]]; then
                COMPREPLY=("${COMPREPLY[@]/#"$HOME"/\~}")
            fi
            return
            ;;
        --completions)
            COMPREPLY=($(compgen -W $'bash\nfish' -- "$cur"))
            return
            ;;
    esac

    compopt -o nospace 2>/dev/null
    local options=$'--in=\n--docs=\n--restore \n--plugins=\n--languages \n--completions=\n--help \n--version '
    COMPREPLY=($(compgen -W "$options" -- "$cur"))
}

complete -F _codecleanup codecleanup
