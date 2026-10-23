rash - start an isolated interactive Bash evaluator
=================================================

*always with you*

Synopsis
--------

.. synopsis::

    rash [--login | --no-login] [--no-startup] [--no-config] [--private]

Description
-----------

Bash wherever this Fish instance is. Rash's experimental Bash evaluator ships inside this Fish executable, ready to launch with ``rash``. There is no separate Bash interpreter to find or install.

On POSIX builds, ``rash`` is a Fish function that launches the current Fish executable with ``--rash-shell``. The child uses the Rust Rash evaluator for Bash input and Fish's editor, completions, prompt, history and terminal handling. Cargo also builds a standalone ``rash`` executable with a portable process host, including native Windows support. Neither form depends on an installed Bash runtime.

In the POSIX Fish editor, Tab completes filenames in ordinary arguments and redirection targets such as ``cat < READ`` or ``echo hi > READ``, including append, clobber and combined-output redirections. Leading redirections, quoted paths and ``~/`` paths are supported. Completion preserves the entered syntax without opening or modifying the target file. Here-documents, here-strings and process substitutions remain outside this completion support.

The following startup and interactive UI behavior describes the POSIX Fish function. The standalone executable and native Windows frontend have their own startup and interactive behavior, described under Windows below.

The child inherits exported variables, the current directory and standard descriptors. Its variables, functions, aliases and directory changes belong to the child. Parent Fish functions and private variables are not shared with Bash evaluation. Use ``exit`` to leave Rash and return to the parent Fish prompt. Fish-only commands such as ``fish_input_mode`` are not Bash builtins in the child.

Normal Fish configuration runs again in the child to configure its interactive UI. Valid ``fish_input_mode`` requests during this initialization are ignored. ``--no-config`` skips Fish configuration and enables private mode. ``--private`` prevents access to saved history and history persistence. The child also inherits private-history mode when it is active in the calling Fish.

Bash startup files are read by default. The child inherits whether the calling Fish is a login shell (``status is-login``):

- From a non-login Fish, Rash reads ``~/.bashrc``.
- From a login Fish, Rash reads ``/etc/profile`` and the first readable ``~/.bash_profile``, ``~/.bash_login`` or ``~/.profile``. It does not additionally read ``~/.bashrc``; a profile can source it, as in Bash.

``--login`` (``-l``) explicitly selects a login shell; ``--no-login`` selects a non-login shell even when Fish is a login shell. ``--no-startup`` skips Bash startup and logout files. It is independent of ``--no-config``, which controls Fish UI configuration. ``--login`` and ``--no-login`` are mutually exclusive. The existing ``--bash-startup`` spelling remains accepted, but is redundant for this function.

Fish still renders the prompt; Rash runs ``PROMPT_COMMAND`` before it. The lower-level ``fish --rash-shell`` invocation continues to require ``--bash-startup`` explicitly and does not inherit a parent login flag. These wrapper defaults do not affect Bash-style Fish input, ordinary Fish startup, or explicit Bash scripts.

``source script.sh`` inside Rash executes Bash code and changes the current child session. For isolated noninteractive script execution, use ``fish --script-language=bash script.sh``. On the POSIX Fish frontend, ``fish --rash-shell`` requires a terminal and does not accept ``-c`` or a script filename. Other supported Fish invocation options, such as ``-C`` for Fish initialization commands, are passed through by the function.

For Bash-style input that runs in the existing Fish session and retains Fish functions and expansion rules, use ``fish_input_mode bash`` instead. That dialect's ``source`` command continues to evaluate Fish code.

Rash is an experimental, incomplete Bash implementation. Unsupported syntax and builtin behavior do not fall back to native Bash; see ``doc_internal/rash-compatibility.md`` in the source tree for the current limits.

Rustty and Tmuxx
---------------

Use the absolute path to this Fish build. Launch a Rash terminal directly in Rustty:

.. code-block:: sh

    rustty -e /path/to/fish --rash-shell --bash-startup

On POSIX, create a native Tmuxx session and display it in Rustty:

.. code-block:: sh

    tmuxx -L fish-rash -f /dev/null new-session -d -s work \
      /path/to/fish --rash-shell --bash-startup
    rustty --tmuxx=local:fish-rash

Add ``--login`` to Fish's arguments to select login profiles. Direct launches have no parent Fish login status to inherit. Substitute ``--interactive-language=bash`` for the Rash flags to use Bash-style Fish input instead. Settings for ordinary Rustty windows and future Tmuxx panes are in ``doc_internal/bash-input-feasibility.md``.

Windows
-------

Cargo builds native ``fish.exe`` and ``rash.exe`` with Rust's Windows toolchain:

.. code-block:: powershell

    cargo build --locked --bins --target x86_64-pc-windows-msvc
    .\target\x86_64-pc-windows-msvc\debug\rash.exe -c 'printf "%s\n" "native Rash"'
    .\target\x86_64-pc-windows-msvc\debug\fish.exe --script-language=bash .\script.sh
    rustty.exe -e C:/src/fish-shell/target/x86_64-pc-windows-msvc/debug/fish.exe --rash-shell

The standalone ``rash`` executable and native ``fish --rash-shell`` accept ``-c``, script files, standard input, and interactive input. On Windows the Unicode line editor supports cursor editing, filename and command completion, and persistent history. ``--private`` (``-P``) keeps history within the current session. Rash history is stored separately from native Fish history and existing POSIX Fish history. The standalone Unix executable retains a simple line-input prompt.

Bash startup files are opt-in with ``--bash-startup``; add ``--login`` for login profiles. ``--no-config`` (``-N``) skips startup files. These native Rash entry points do not load Fish UI configuration or run ``fish_prompt``. Programmable Fish completion, autosuggestions and signal traps remain unavailable.

Both native shells support concurrent pipelines and background jobs. ``jobs``, ``wait``, ``fg``, ``bg``, ``disown`` and ``kill`` operate on jobs owned by the shell. On Windows, Ctrl-C cancels foreground work; ``kill -STOP %1`` suspends an owned job and ``bg`` or ``fg`` resumes it. Windows has no POSIX terminal foreground process groups, and a background job's input cannot be reassigned by ``fg``. Native paths and arguments use Unicode and external processes run through the Windows process API.

The MSYS2/Cygwin target and WSL retain the full POSIX Fish host. Select the backend with Cargo's target and the corresponding toolchain; there is no MSYS2 feature toggle. Native Cargo builds do not yet provide CMake installation or packaging. See ``doc_internal/native-windows.md`` for build commands, supported native Fish syntax, isolated tmuxx launch examples and remaining work.

Rash supplies an interpreter, not Git or the external tools a script may invoke, and its Bash compatibility remains incomplete.
