complete -c rash -f
complete -c rash -s h -l help -d 'Display help and exit'
complete -c rash -s l -l login -d 'Run as a login shell (inherited from Fish by default)'
complete -c rash -l no-login -d 'Run as a non-login shell'
complete -c rash -s N -l no-config -d 'Do not read Fish configuration'
complete -c rash -s P -l private -d 'Do not persist history'
complete -c rash -l no-startup -d 'Skip Bash startup files'
complete -c rash -s C -l init-command -x -a '(__fish_complete_command)' -d 'Run Fish code after configuration'
