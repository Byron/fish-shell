# localization: skip(private)
# Fish's helpers need Unix find options even when Windows find.exe is on PATH.
function __fish_find
    set -l utility __fish_native_utility
    if builtin -q $utility; and builtin $utility --is-enabled
        builtin $utility find $argv
    else
        command find $argv
    end
end
