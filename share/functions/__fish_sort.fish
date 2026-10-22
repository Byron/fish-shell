# localization: skip(private)
# Fish's helpers need Unix sort options even when Windows sort.exe is on PATH.
function __fish_sort
    set -l utility __fish_native_utility
    if builtin -q $utility; and builtin $utility --is-enabled
        builtin $utility sort $argv
    else
        command sort $argv
    end
end
