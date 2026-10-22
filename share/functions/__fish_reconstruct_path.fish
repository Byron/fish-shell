# Track only entries added by this handler, preserving inherited PATH entries.
set -qg __fish_added_user_paths
or set -g __fish_added_user_paths

function __fish_reconstruct_path -d "Update PATH when fish_user_paths changes" --on-variable fish_user_paths
    set -l separator :
    status is-native-windows
    and set separator ';'

    # Deduplicate $fish_user_paths.
    # This should help with people appending to it in config.fish.
    set -l new_user_path
    for path in (string split $separator -- $fish_user_paths)
        if not contains -- $path $new_user_path
            set -a new_user_path $path
        end
    end

    if test (count $new_user_path) -lt (count $fish_user_paths)
        # This will end up calling us again, so we return.
        set fish_user_paths $new_user_path
        return
    end

    set -l local_path $PATH

    for x in $__fish_added_user_paths
        set -l idx (contains --index -- $x $local_path)
        and set -e local_path[$idx]
    end

    set -g __fish_added_user_paths
    if set -q fish_user_paths
        # Explicitly split because fish_user_paths might not be a path variable,
        # but PATH definitely is. Drive-letter colons are preserved on Windows.
        for x in (string split $separator -- $fish_user_paths[-1..1])
            if set -l idx (contains --index -- $x $local_path)
                set -e local_path[$idx]
            else
                set -ga __fish_added_user_paths $x
            end
            set -p local_path $x
        end
    end

    set -xg PATH $local_path
end
