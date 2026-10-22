__fish_make_completion_signals

set -l signal_option -s s
if status is-native-windows
    set -a signal_option -l signal
end

for sig in $__kill_signals[-1..1]
    set -l number (string split ' ' $sig)[1]
    set -l name (string split ' ' $sig)[2]
    complete -c kill -o $number -d $name
    complete -c kill -o $name -d $number
    complete -c kill -k $signal_option -x -a "$name\t$number"
end

if status is-native-windows
    complete -c kill -xa '(jobs -p)'
    complete -c kill -s s -l signal -d "Signal to send"
    complete -c kill -s l -l list -d "List supported signal names or convert a name or number"
    return
end

complete -c kill -xa '(__fish_complete_pids)'

if kill -L >/dev/null 2>/dev/null
    complete -c kill -s s -l signal -d "Signal to send"
    complete -c kill -s l -l list -d "List signal names"
    complete -c kill -s L -l table -d "List signal names and their #s"
    complete -c kill -s a -l all -d "Resolve PIDs from other users' too"
    complete -c kill -s p -l pid -d "Show resolved PID, don't send signal"
    complete -c kill -s q -l queue -d "Use sigqueue(2) rather than kill(2)"
    complete -c kill -l verbose -d "Print PIDs and signals sent"
else # non-GNU
    complete -c kill -s s -d "Signal to send"
    complete -c kill -s l -d "List signal names"
end
