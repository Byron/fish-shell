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

``rash`` is a Fish function that launches the current Fish executable with ``--rash-shell``. There is no separate installed Rash executable or native Bash runtime dependency. The child uses the Rust Rash evaluator for Bash input and Fish's editor, completions, prompt, history and terminal handling.

The child inherits exported variables, the current directory and standard descriptors. Its variables, functions, aliases and directory changes belong to the child. Parent Fish functions and private variables are not shared with Bash evaluation. Use ``exit`` to leave Rash and return to the parent Fish prompt. Fish-only commands such as ``fish_input_mode`` are not Bash builtins in the child.

Normal Fish configuration runs again in the child to configure its interactive UI. Valid ``fish_input_mode`` requests during this initialization are ignored. ``--no-config`` skips Fish configuration and enables private mode. ``--private`` prevents access to saved history and history persistence. The child also inherits private-history mode when it is active in the calling Fish.

Bash startup files are read by default. The child inherits whether the calling Fish is a login shell (``status is-login``):

- From a non-login Fish, Rash reads ``~/.bashrc``.
- From a login Fish, Rash reads ``/etc/profile`` and the first readable ``~/.bash_profile``, ``~/.bash_login`` or ``~/.profile``. It does not additionally read ``~/.bashrc``; a profile can source it, as in Bash.

``--login`` (``-l``) explicitly selects a login shell; ``--no-login`` selects a non-login shell even when Fish is a login shell. ``--no-startup`` skips Bash startup and logout files. It is independent of ``--no-config``, which controls Fish UI configuration. ``--login`` and ``--no-login`` are mutually exclusive. The existing ``--bash-startup`` spelling remains accepted, but is redundant for this function.

Fish still renders the prompt; Rash runs ``PROMPT_COMMAND`` before it. The lower-level ``fish --rash-shell`` invocation continues to require ``--bash-startup`` explicitly and does not inherit a parent login flag. These wrapper defaults do not affect Bash-style Fish input, ordinary Fish startup, or explicit Bash scripts.

``source script.sh`` inside Rash executes Bash code and changes the current child session. For isolated noninteractive script execution, use ``fish --script-language=bash script.sh``. The Rash shell itself requires a terminal and does not accept ``-c`` or a script filename. Other supported Fish invocation options, such as ``-C`` for Fish initialization commands, are passed through.

For Bash-style input that runs in the existing Fish session and retains Fish functions and expansion rules, use ``fish_input_mode bash`` instead. That dialect's ``source`` command continues to evaluate Fish code.

Rash is an experimental, incomplete Bash implementation. Unsupported syntax and builtin behavior do not fall back to native Bash; see ``doc_internal/rash-compatibility.md`` in the source tree for the current limits.

Rustty and Tmuxx
---------------

Use the absolute path to this Fish build. Launch a Rash terminal directly in Rustty:

.. code-block:: sh

    rustty -e /path/to/fish --rash-shell --bash-startup

Or create a native Tmuxx session and display it in Rustty:

.. code-block:: sh

    tmuxx -L fish-rash -f /dev/null new-session -d -s work \
      /path/to/fish --rash-shell --bash-startup
    rustty --tmuxx=local:fish-rash

Add ``--login`` to Fish's arguments to select login profiles. Direct launches have no parent Fish login status to inherit. Substitute ``--interactive-language=bash`` for the Rash flags to use Bash-style Fish input instead. Settings for ordinary Rustty windows and future Tmuxx panes are in ``doc_internal/bash-input-feasibility.md``.

Windows
-------

Rash makes a separately installed Bash interpreter unnecessary wherever this Fish build runs. On Windows, one route is to build or install this modified Fish inside WSL, then launch it from Rustty. For example, with Fish installed at ``/usr/local/bin/fish`` in the Ubuntu distribution:

.. code-block:: powershell

    rustty.exe -e wsl.exe --distribution Ubuntu --exec /usr/local/bin/fish --rash-shell --bash-startup

This WSL example does not require Git Bash, but has not been tested here. The current Rash core and Fish host still depend on Unix APIs; native Windows support needs further portability work. Rash supplies an interpreter, not Git or the external tools a script may invoke, and its Bash compatibility remains incomplete.
