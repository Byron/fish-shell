#RUN: %fish -i %s
#REQUIRES: command -v git

# Git creates fixtures; a forwarding shim records calls made by fish's fallback.
set -l git_exe (command -s git)
for name in (set -nx | string match 'GIT_*')
    set -e $name
end
# Avoid macOS's /usr/bin/git developer-tool launcher after locating Git.
set -l git_exec_path (command $git_exe --exec-path 2>/dev/null)
set -l direct_git (path resolve -- "$git_exec_path/../../bin/git")
test -x "$direct_git"; and set git_exe "$direct_git"
set -gx GIT_CONFIG_GLOBAL /dev/null
set -gx GIT_CONFIG_NOSYSTEM 1
set -gx GIT_AUTHOR_NAME 'Git integration test'
set -gx GIT_AUTHOR_EMAIL git@example.invalid
set -gx GIT_COMMITTER_NAME $GIT_AUTHOR_NAME
set -gx GIT_COMMITTER_EMAIL $GIT_AUTHOR_EMAIL

set -l tmp (mktemp -d)
mkdir "$tmp/repo" "$tmp/empty" "$tmp/shim" "$tmp/outside"
cd "$tmp/repo"
command $git_exe init -q
command $git_exe symbolic-ref HEAD refs/heads/main
printf 'original\n' >tracked.txt
command $git_exe add tracked.txt
command $git_exe commit -qm initial
command $git_exe remote add origin .
command $git_exe update-ref refs/remotes/origin/main HEAD
command $git_exe config branch.main.remote origin
command $git_exe config branch.main.merge refs/heads/main
command $git_exe config alias.inspect show
printf 'second\n' >>tracked.txt
command $git_exe commit -qam second
command $git_exe branch feature/example
command $git_exe tag release-example
printf 'saved\n' >>tracked.txt
command $git_exe stash push -qm saved
printf 'modified\n' >>tracked.txt
printf 'staged\n' >staged.txt
command $git_exe add staged.txt
printf 'untracked\n' >untracked.txt

set -l expected_status "$(command $git_exe status --porcelain -z | string split0 | string join \n)"
set -l expected_count (command $git_exe rev-list --count --left-right '@{upstream}...HEAD')
set -l expected_stash (command $git_exe stash list --format=%gd:%gs)
set -l expected_head (command $git_exe rev-parse HEAD)
set -l enabled 0
builtin __fish_git --is-enabled; and set enabled 1

function require -a label
    if not $argv[2..-1]
        printf 'FAIL: %s\n' $label
        exit 1
    end
end

begin
    set -lx PATH "$tmp/empty"
    if test $enabled = 1
        require 'prompt without Git on PATH' string match -q '*main*' (fish_git_prompt)
        require 'reference completion without Git on PATH' string match -q 'feature/example*' (complete -C 'git reset feature/')
        require 'tag completion without Git on PATH' string match -q 'release-example*' (complete -C 'git reset release-')
    else
        require 'disabled prompt' test -z "$(fish_git_prompt)"
        require 'disabled reference completion' test -z "$(complete -C 'git reset feature/')"
    end
    require 'static completions without Git' string match -q 'reset*' (complete -C 'git rese')
end

set -gx FISH_GIT_TRACE "$tmp/invoked"
set -gx FISH_GIT_HELPER_TRACE "$tmp/helper-invoked"
set -gx FISH_TEST_GIT_EXE "$git_exe"
printf '%s\n' '#!/bin/sh' 'printf invoked >> "$FISH_GIT_TRACE"' 'exec "$FISH_TEST_GIT_EXE" "$@"' >"$tmp/shim/git"
printf '%s\n' '#!/bin/sh' 'printf invoked >> "$FISH_GIT_HELPER_TRACE"' 'exit 91' >"$tmp/shim/reject-helper"
chmod +x "$tmp/shim/git" "$tmp/shim/reject-helper"
if test $enabled = 1
    command $git_exe config core.fsmonitor "$tmp/shim/reject-helper"
    command $git_exe config diff.external "$tmp/shim/reject-helper"
end
set -p PATH "$tmp/shim"

require 'repository detection' __fish_is_git_repository
require HEAD test "$expected_head" = "$(__fish_git_query rev-parse HEAD)"
require configuration test show = "$(__fish_git_query config alias.inspect)"
require status test "$expected_status" = "$(__fish_git_query status --porcelain -z | string split0 | string join \n)"
require 'upstream counts' test "$expected_count" = "$(__fish_git_query rev-list --count --left-right '@{upstream}...HEAD')"
require 'stash list' test "$expected_stash" = "$(__fish_git_query stash list --format=%gd:%gs)"
require 'history completion' string match -q '*second*' (complete -C 'git reset ')
require 'reference completion' string match -q 'feature/example*' (complete -C 'git reset feature/')
require 'tag completion' string match -q 'release-example*' (complete -C 'git reset release-')
require 'staged-file completion' string match -q 'staged.txt*' (complete -C 'git restore --staged staged')
require 'untracked-file completion' string match -q 'untracked.txt*' (complete -C 'git add untracked')
require 'remote completion' string match -q 'origin*' (complete -C 'git fetch ori')
require 'alias completion' string match -q 'inspect*' (complete -C 'git ins')
require 'tree-file completion' string match -q 'HEAD:tracked.txt*' (complete -C 'git show HEAD:tr')

set -g __fish_git_prompt_show_informative_status 1
set -g __fish_git_prompt_showuntrackedfiles 1
set -g __fish_git_prompt_showstashstate 1
set -g __fish_git_prompt_showupstream informative
set -l prompt (fish_git_prompt)
require 'informative branch' string match -q '*main*' $prompt
require 'informative staged count' string match -q '*●1*' $prompt
require 'informative dirty count' string match -q '*✚1*' $prompt
require 'informative untracked count' string match -q '*…1*' $prompt
require 'informative stash count' string match -q '*⚑1*' $prompt

begin
    set -lx GIT_INDEX_FILE "$tmp/alternate-index"
    command $git_exe -c core.fsmonitor= read-tree --empty
    require 'alternate index' test -z "$(__fish_git_query ls-files)"
end
require 'default index retained' contains -- tracked.txt (__fish_git_query ls-files)

mkdir "$tmp/alternate tree"
printf 'alternate\n' >"$tmp/alternate tree/only-there.txt"
command $git_exe config core.worktree "$tmp/alternate tree"
require 'configured worktree root' test (path resolve "$tmp/alternate tree") = "$(__fish_git_query rev-parse --show-toplevel)"
require 'configured worktree status' contains -- '?? only-there.txt' (__fish_git_query status --porcelain)
require 'explicit worktree takes precedence' test "$PWD" = "$(__fish_git_query --work-tree "$PWD" rev-parse --show-toplevel)"
command $git_exe config --unset core.worktree
require 'explicit worktree status' contains -- '?? only-there.txt' (__fish_git_query --work-tree "$tmp/alternate tree" status --porcelain)

require 'configured helpers not executed' test ! -e "$FISH_GIT_HELPER_TRACE"
if test $enabled = 1
    require 'ordinary queries stay in gix' test ! -e "$FISH_GIT_TRACE"
    command $git_exe config --unset-all core.fsmonitor
    command $git_exe config --unset-all diff.external
else
    require 'disabled feature uses Git' test -s "$FISH_GIT_TRACE"
end

# Ordinary misses are not missing library functionality.
command rm -f "$FISH_GIT_TRACE"
require 'no annotated tag' test -z "$(__fish_git_query describe HEAD 2>/dev/null)"
__fish_git_query describe missing-revision >/dev/null 2>/dev/null
set -l missing_status $status
require 'missing revision fails' test $missing_status != 0
cd "$tmp/outside"
require 'outside repository query' test -z "$(__fish_git_query rev-parse HEAD 2>/dev/null)"
require 'outside repository prompt' test -z "$(fish_git_prompt)"
if test $enabled = 1
    require 'ordinary misses stay in gix' test ! -e "$FISH_GIT_TRACE"
end
cd "$tmp/repo"

command rm -f "$FISH_GIT_TRACE"
begin
    set -lx GIT_COMMON_DIR "$PWD/.git"
    if test $enabled = 1
        builtin __fish_git rev-parse HEAD >/dev/null
        set -l native_status $status
        require 'common-directory override is unsupported' test $native_status = 125
    end
    require 'common-directory fallback HEAD' test "$expected_head" = "$(__fish_git_query rev-parse HEAD)"
end
require 'common-directory fallback reaches Git' test -s "$FISH_GIT_TRACE"

# A clean filter must retain Git's normalization semantics.
printf '%s\n' '#!/bin/sh' 'exec tr a-z A-Z' >"$tmp/shim/clean-filter"
chmod +x "$tmp/shim/clean-filter"
printf 'filtered.txt filter=integration\n' >.gitattributes
command $git_exe config filter.integration.clean "$tmp/shim/clean-filter"
printf 'one\n' >filtered.txt
command $git_exe add .gitattributes filtered.txt
printf 'oNe\n' >filtered.txt
set expected_status "$(command $git_exe status --porcelain -z | string split0 | string join \n)"
command rm -f "$FISH_GIT_TRACE"
require 'clean-filter status' test "$expected_status" = "$(__fish_git_query status --porcelain -z | string split0 | string join \n)"
if test $enabled = 1
    require 'clean-filter status stays in gix' test ! -e "$FISH_GIT_TRACE"
else
    require 'disabled clean-filter status uses Git' test -s "$FISH_GIT_TRACE"
end

command $git_exe tag -am inner inner "$expected_head"
command $git_exe tag -am outer outer inner 2>/dev/null
command $git_exe update-ref -d refs/tags/inner
set -l expected_describe (command $git_exe describe HEAD)
command rm -f "$FISH_GIT_TRACE"
if test $enabled = 1
    # The pinned gix version does not select nested annotated tags by default.
    require 'nested annotated tag follows gix result' test -z "$(__fish_git_query describe HEAD)"
    require 'nested annotated tag stays in gix' test ! -e "$FISH_GIT_TRACE"
else
    require 'disabled nested annotated tag uses Git result' test "$expected_describe" = "$(__fish_git_query describe HEAD)"
    require 'disabled nested annotated tag reaches Git' test -s "$FISH_GIT_TRACE"
end

# Unsupported storage must not retry Git when gix is enabled.
if command $git_exe init -q --ref-format=reftable "$tmp/reftable" 2>/dev/null
    cd "$tmp/reftable"
    command $git_exe commit --allow-empty -qm initial
    set -l reftable_head (command $git_exe rev-parse HEAD)
    command rm -f "$FISH_GIT_TRACE"
    if test $enabled = 1
        __fish_git_query rev-parse HEAD >"$tmp/reftable-output" 2>/dev/null
        set -l query_status $status
        require 'reftable query fails' test $query_status != 0
        require 'reftable query has no result' test ! -s "$tmp/reftable-output"
        require 'reftable query stays in gix' test ! -e "$FISH_GIT_TRACE"
    else
        require 'disabled reftable query HEAD' test "$reftable_head" = "$(__fish_git_query rev-parse HEAD)"
        require 'disabled reftable query uses Git' test -s "$FISH_GIT_TRACE"
    end
else
    command $git_exe config core.repositoryformatversion 1
    command $git_exe config extensions.refStorage reftable
    command rm -f "$FISH_GIT_TRACE"
    if test $enabled = 1
        __fish_git_query rev-parse HEAD >"$tmp/reftable-output" 2>/dev/null
        set -l query_status $status
        require 'unsupported storage query fails' test $query_status != 0
        require 'unsupported storage query has no result' test ! -s "$tmp/reftable-output"
        require 'unsupported storage query stays in gix' test ! -e "$FISH_GIT_TRACE"
    else
        __fish_git_query rev-parse HEAD >/dev/null 2>/dev/null
        require 'disabled unsupported storage query uses Git' test -s "$FISH_GIT_TRACE"
    end
end

cd "$tmp/outside"
require 'outside repository' test -z "$(fish_git_prompt)"
echo 'gix integration checks passed'
#CHECK: gix integration checks passed
