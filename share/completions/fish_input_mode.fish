complete -c fish_input_mode -f
complete -c fish_input_mode -n 'not __fish_seen_subcommand_from fish bash' -a fish -d 'Use Fish syntax for subsequent interactive input'
complete -c fish_input_mode -n 'not __fish_seen_subcommand_from fish bash' -a bash -d 'Use Bash-style syntax with Fish evaluation'
