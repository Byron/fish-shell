# localization: skip(private)
# Fish's helpers need Unix sort options even when Windows sort.exe is on PATH.
function __fish_sort
    if builtin -q __fish_native_utility; and builtin __fish_native_utility --is-enabled
        builtin __fish_native_utility sort $argv
    else
        command sort $argv
    end
end
