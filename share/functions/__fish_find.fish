# localization: skip(private)
# Fish's helpers need Unix find options even when Windows find.exe is on PATH.
function __fish_find
    if builtin -q __fish_native_utility; and builtin __fish_native_utility --is-enabled
        builtin __fish_native_utility find $argv
    else
        command find $argv
    end
end
