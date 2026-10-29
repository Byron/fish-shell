# RUN: %fish %s
# The helper uses only Fish builtins, including on hosts without awk.
function type
    return 1
end
complete -c concat_probe -f -a '(printf "%s\n" alpha beta gamma | __fish_concat_completions)'
complete -C 'concat_probe al'
# CHECK: alpha
complete -C 'concat_probe alpha,b'
# CHECK: alpha,beta
complete -C 'concat_probe alpha,beta,g'
# CHECK: alpha,beta,gamma
