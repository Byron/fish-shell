#RUN: fish=%fish %fish %s

$fish --interactive-language=bash -N -c 'set x fish; echo $x'
# CHECK: fish

$fish --interactive-language=bash -N -C 'set -g x init' -c 'function fish_function; echo $x; end; fish_function'
# CHECK: init

printf 'set x script; echo $x\n' > bash-input-script.fish
$fish --interactive-language=bash -N bash-input-script.fish
# CHECK: script
rm bash-input-script.fish

$fish --interactive-language=fish -N -c 'echo default'
# CHECK: default

$fish --interactive-language=unknown -c exit
echo $status
# CHECKERR: fish: --interactive-language expects fish or bash
# CHECK: 2

$fish -N -c 'builtin -q bash_session; echo obsolete-builtin:$status; builtin -q fish_input_mode; echo input-mode:$status; complete -C bash_sess'
# CHECK: obsolete-builtin:1
# CHECK: input-mode:0

for args in '-c true' '--script-language=bash example.sh' '--interactive-language=bash' '-n' example.sh
    $fish -N --rash-shell (string split ' ' -- $args) 2>/dev/null
    echo rash-conflict:$status
end
# CHECK: rash-conflict:2
# CHECK: rash-conflict:2
# CHECK: rash-conflict:2
# CHECK: rash-conflict:2
# CHECK: rash-conflict:2

$fish -N --rash-shell </dev/null
echo rash-nontty:$status
# CHECKERR: fish: --rash-shell requires a terminal on standard input
# CHECK: rash-nontty:2

$fish -N --interactive-language=bash --bash-startup </dev/null
echo dialect-startup:$status
# CHECKERR: fish: --bash-startup requires --rash-shell or a Bash script
# CHECK: dialect-startup:2

$fish -N -c 'fish_input_mode --bash-startup bash'
echo removed-startup-option:$status
# CHECKERR: fish_input_mode: expected fish|bash
# CHECK: removed-startup-option:2

$fish -N -c 'rash -c "echo must-not-run"'
echo rash-function:$status
# CHECKERR: fish: --rash-shell cannot be combined with -c, a script file, --script-language, --interactive-language or -n
# CHECK: rash-function:2
