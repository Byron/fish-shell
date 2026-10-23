# Native reader customization

The Windows reader runs single-key `bind` callbacks in the current Fish session.
Callbacks can call functions, change variables or directories, and invoke normal
commands. Standard input is isolated from the editor; output and diagnostics are
printed without treating them as input. Ctrl-C cancels callback work.

```fish
function insert_git_status
    commandline --replace 'git status'
    commandline --function execute
end
bind ctrl-g insert_git_status
bind alt-s 'commandline --insert " --short"'
```

`commandline` can replace, insert into, or append to the current buffer, job,
process, or token. `--cursor` reads or changes a character position; Unicode byte
offsets are handled internally. Multiple values are joined with newlines. The
callback sees its edits immediately through subsequent `commandline` queries.
The finished edit is applied as one undoable change before queued input functions
run. `commandline --function` accepts the names listed by `bind --function-names`.
Completion generators and command substitutions can query their input but cannot
modify the outer reader.

Single-key bindings accept multiple callback strings and input functions. Callback
strings run in order, then queued input functions run in order. Changes to binding
registrations take effect on the next prompt. Custom binding modes and multikey
sequences are still unavailable. Terminal interaction by external full-screen
programs from a binding is not supported: use a submitted command instead.

Validation includes semantic tests for Unicode edits, lexical extents, session
changes and nested completion isolation, plus native ConPTY tests for callbacks,
worker pipelines, queued execution and undo.
