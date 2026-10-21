#RUN: fish=%fish %fish %s

set -l tmp (mktemp -d)
set -l original_pwd $PWD
set -gx SCRIPT_EXPORT parent
set -g SCRIPT_LOCAL fish-only
set -l script '-script ü;$(touch INJECTED).bash'
printf '%s\n' \
    '[[ $0 == "$EXPECTED_SCRIPT" ]] || exit 42' \
    'printf "args:%s:%s:%s\n" "$#" "$1" "$2"' \
    'printf "environment:%s:%s\n" "$SCRIPT_EXPORT" "${SCRIPT_LOCAL-unset}"' \
    'export SCRIPT_EXPORT=child; cd /; function child_function { :; }; alias child_alias=true' \
    'exit 37' >$tmp/$script
pushd $tmp
EXPECTED_SCRIPT=$script $fish -N --script-language=bash -- $script 'a "quote"; $HOME' 'ü * ?'
# CHECK: args:2:a "quote"; $HOME:ü * ?
# CHECK: environment:parent:unset
echo status:$status
# CHECK: status:37
test ! -e INJECTED; and echo argument-text-preserved
# CHECK: argument-text-preserved
test "$SCRIPT_EXPORT" = parent; and test "$PWD" = "$tmp"; and echo state-isolated
# CHECK: state-isolated
popd
test "$PWD" = "$original_pwd"; and echo cwd-preserved
# CHECK: cwd-preserved

printf '%s\n' 'printf "export:%s\n" "$SCRIPT_EXPORT"' >$tmp/configured.bash
$fish -N --script-language=bash -C 'set -gx SCRIPT_EXPORT configured' $tmp/configured.bash
# CHECK: export:configured

printf '%s\n' 'export STARTUP=loaded; printf "startup\n"' >$tmp/bash_env
printf '%s\n' 'printf "startup-state:%s\n" "${STARTUP-unset}"' >$tmp/startup.bash
BASH_ENV=$tmp/bash_env $fish -N --script-language=bash $tmp/startup.bash
# CHECK: startup-state:unset
BASH_ENV=$tmp/bash_env $fish -N --script-language=bash --bash-startup $tmp/startup.bash
# CHECK: startup
# CHECK: startup-state:loaded

printf '%s\n' 'touch "$MARKER"; value=$(touch "$EXPANSION")' >$tmp/parse.bash
MARKER=$tmp/marker EXPANSION=$tmp/expansion $fish -N -n --script-language=bash \
    -C 'echo postconfig-must-not-run' $tmp/parse.bash
echo syntax-status:$status
# CHECK: syntax-status:0
test ! -e $tmp/marker; and test ! -e $tmp/expansion; and echo syntax-did-not-execute
# CHECK: syntax-did-not-execute
printf '%s\n' 'if true; then' >$tmp/invalid.bash
$fish -N -n --script-language=bash $tmp/invalid.bash 2>/dev/null
echo invalid-status:$status
# CHECK: invalid-status:2

printf '%s\n' 'IFS= read -r line; printf "read:%s\n" "$line"; printf "script-stderr\n" >&2' >$tmp/io.bash
printf '%s\n' 'stdin data' | $fish -N --script-language=bash $tmp/io.bash
# CHECK: read:stdin data
# CHECKERR: script-stderr

printf '%s\n' 'set language fish; echo $language' >$tmp/default.fish
$fish -N --script-language=fish $tmp/default.fish
# CHECK: fish
$fish -N --script-language=bash -c 'echo must-not-run' $tmp/default.fish 2>/dev/null
echo invalid-combination:$status
# CHECK: invalid-combination:2
$fish -N --script-language=bash </dev/null 2>/dev/null
echo missing-file:$status
# CHECK: missing-file:2
$fish -N --script-language=fish </dev/null 2>/dev/null
echo fish-missing-file:$status
# CHECK: fish-missing-file:2
$fish -N --script-language=unknown $tmp/default.fish
echo unknown-language:$status
# CHECKERR: fish: --script-language expects fish or bash
# CHECK: unknown-language:2
$fish -N --script-language=fish -c 'echo must-not-run' $tmp/default.fish 2>/dev/null
echo fish-invalid-combination:$status
# CHECK: fish-invalid-combination:2
$fish -N -i --script-language=bash $tmp/default.fish
echo interactive-file:$status
# CHECKERR: fish: --script-language=bash runs a noninteractive script and cannot be combined with -i
# CHECK: interactive-file:2

$fish -N --bash-startup -c 'echo must-not-run'
echo startup-fish-command:$status
# CHECKERR: fish: --bash-startup requires --rash-shell or a Bash script
# CHECK: startup-fish-command:2
$fish -N --bash-startup --script-language=fish $tmp/default.fish 2>/dev/null
echo startup-fish-file:$status
# CHECK: startup-fish-file:2
$fish -N --bash-startup --interactive-language=bash -c 'echo must-not-run' 2>/dev/null
echo startup-command:$status
# CHECK: startup-command:2
$fish -N --bash-startup --interactive-language=bash </dev/null 2>/dev/null
echo startup-nontty:$status
# CHECK: startup-nontty:2
$fish -N -i --bash-startup --interactive-language=bash </dev/null 2>/dev/null
echo startup-forced-nontty:$status
# CHECK: startup-forced-nontty:2

$fish -N --interactive-language=bash -c 'set language fish; echo command-language:$language'
# CHECK: command-language:fish
$fish -N --interactive-language=bash -C 'set -g language configured-fish' -c 'echo command-language:$language'
# CHECK: command-language:configured-fish

rm -rf $tmp
