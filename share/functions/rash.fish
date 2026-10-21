function rash --description 'Start an isolated Rash shell using this Fish executable'
    # Preserve Fish option arguments, even when their values look like wrapper options.
    argparse --strict-longopts --move-unknown --unknown-arguments=none --stop-nonopt \
        --exclusive login,no-login --exclusive bash-startup,no-startup \
        'h/help&' 'l/login&' 'no-login&' 'no-startup&' 'bash-startup&' \
        c/command= C/init-command= f/features= d/debug= o/debug-output= \
        D/debug-stack-frames= p/profile= profile-startup= \
        interactive-language= script-language= -- $argv
    or return

    if set -ql _flag_help
        __fish_print_help rash
        return
    end

    set -l startup --bash-startup
    set -ql _flag_no_startup; and set startup
    set -l login
    if set -ql _flag_login; or begin
            not set -ql _flag_no_login; and status is-login
        end
        set login --login
    end
    if test -n "$fish_private_mode"
        set -a argv_opts --private
    end
    set -l rash_fish (status fish-path)
    command $rash_fish --rash-shell $startup $login $argv_opts -- $argv
end
