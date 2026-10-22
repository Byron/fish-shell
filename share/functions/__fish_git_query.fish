function __fish_git_query
    builtin __fish_git $argv
    set -l query_status $status
    if test $query_status -eq 125
        command -sq git
        or return 1
        command git $argv
    else
        return $query_status
    end
end
