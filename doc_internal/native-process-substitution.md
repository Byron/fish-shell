# Native process substitution

Native Windows supports file-backed `psub`, including `--file` and `--suffix`.
The bundled function shares argument parsing with POSIX Fish and selects a
private native backend to own temporary files. No external `cat`, `mktemp`,
`rm`, or `rmdir` is required, including with `native-utilities` disabled.

For example, ordinary Starship initialization now works:

```fish
starship init fish | source
```

The file stays available until the command that consumes the substitution
returns. This includes functions, external commands, `source`, pipelines and
background commands. Assigning a substitution filename with `set` consumes it:
the file is removed after `set` returns, just as on POSIX. Nested substitutions
use their nearest consuming command. Input is copied as bytes in bounded chunks,
so binary data and streams larger than the shell's capture limit are supported.

Files live in private subdirectories of the Windows temporary directory, selected
by the usual `TMP`/`TEMP` environment settings. Interpreter workers share their
caller's directory; the owning shell also keeps a cleanup guard for each worker.
Normal completion, expansion errors and owned-job cancellation remove these
directories. Stopped jobs retain their files until the job completes after a
resume or termination; an enclosing function can continue using the same file
until it returns. Completed background jobs release their temporary resources
without requiring `wait`. Disowned workers also remove their empty scratch
directories after completion when the originating shell has already exited.
If a worker itself exits before a disowned inner job releases an inherited
file, its nonempty scratch directory can remain; worker cleanup never removes
those live files. Abrupt termination of the shell itself can also leave files,
as with other temporary files on Windows.

FIFO mode (`--fifo`) reports an explicit unsupported-mode error. General
`function --on-job-exit` and `--on-process-exit` subscriptions remain separate
work; file cleanup does not depend on emulating those event interfaces. Suffixes
may not contain directory separators or a colon on Windows.
