# localization: skip(private)
#
# This function is intended to be used to generate completions in `-a "(...)"` calls for arguments that take 1..n values
# separated by commas and we dynamically or statically generated a list of n values that must be combined and permuted
# in order to appease both the `complete` machinery and the target application.
#
# TODO: Make this work with completions that have their description appended after a \t

function __fish_concat_completions -d "Generate completions that are specified as comma-separated values from stdin source"
    # The completion engine adds an attached option prefix back to each value.
    set -l token (commandline -ct | string replace -r -- '^-[^=]*=' '')
    # We want to filter out suggestions the user has already entered.
    set -l extant (string split -- ',' $token)
    set -l filter (printf '^%s$\n' (printf '%s\n' $extant | string escape --style=regex) | string join '|')
    # Work around the insanity of trying to read from stdin within a function. Note that we can't place the
    # `read` call in between () to capture the output because that breaks its connection to stdin.
    while read -l line
        echo $line
    end | string match -er '.' | string match -rv -- $filter | string replace -r '^' -- (string replace -rf -- '^(.+),.*$' '$1,' $token; or echo "")

end
