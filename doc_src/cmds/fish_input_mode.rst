fish_input_mode - select the syntax of subsequent interactive input
=================================================================

Synopsis
--------

.. synopsis::

    fish_input_mode fish
    fish_input_mode bash

Description
-----------

``fish_input_mode`` selects Fish syntax or experimental Bash-style Fish syntax for subsequent interactive input. Both modes execute within the same Fish session and use Fish expansion rules. Fish functions, aliases, variables, hooks, jobs and directory changes remain available when switching.

Bash-style Fish input supports a subset of Bash spellings. It is not Bash evaluation: in particular, unquoted variables use Fish's expansion rules, without Bash word splitting. Unsupported constructs report errors before the submission executes. Fish never silently sends the input to a Bash evaluator.

Backticks are ordinary characters, including inside strings, as in Fish. They do not execute command substitutions or require a matching backtick. ``$(...)`` command substitutions and special variables such as ``$?`` remain supported. Leading unquoted ``~/`` paths support completion and suggestions while preserving the ``~/`` spelling.

Tab completes filenames in arguments and redirection targets, including ``<``, ``>``, ``>>``, ``>|``, ``&>`` and ``&>>``, with optional file descriptors. For example, ``echo hi > READ`` can complete to ``echo hi > README.rst``. Redirections can precede the command, and spaces around them are optional. Completion preserves quoting and the rest of the input without opening or modifying the target file.

The change takes effect after the entire submitted command finishes. For example, in Fish::

    fish_input_mode bash; set -g example still_fish

Both commands execute as Fish. The next input accepts Bash-style syntax. ``fish_input_mode fish`` switches subsequent input back to ordinary Fish syntax. Fish-defined functions, hooks, key bindings, configuration files, :doc:`source`, script files, ``fish -c`` and ``fish -C`` always retain Fish syntax.

The builtin can also select the initial input syntax from interactive startup configuration. The rest of that configuration remains Fish code. For example, in ``config.fish``::

    if status is-interactive
        fish_input_mode bash
    end

Requests from prompts, key bindings, completion providers, event hooks, command substitutions and nested interactive readers are refused. These contexts cannot change the syntax of a buffer being edited.

Session state and history
-------------------------

Switching changes only the interactive input syntax. There is one Fish session; variables, functions, exit status, running and stopped jobs, and terminal ownership are preserved. Both input modes share the existing Fish history, including the current history session and private mode. Entries retain their entered text; suggestions are checked using the active syntax, and recalled commands using the other syntax may need editing. Rash keeps its separate history.

History files from earlier builds under ``bash-style-history/`` are preserved but are not automatically imported into the shared history.

To evaluate Bash with its own expansion rules and session state, use :doc:`rash`. That command launches a dedicated child shell using the same Fish executable. It cannot switch input mode in place; use ``exit`` to return to the parent. Valid ``fish_input_mode`` requests in the child's Fish startup configuration are ignored so ordinary Fish UI configuration can be reused.

Bash startup files belong to the dedicated Rash shell; ``rash`` reads them by default and ``rash --no-startup`` skips them. They are not read by ``fish_input_mode bash``. ``source`` at the Bash-style Fish prompt continues to source Fish code; ``source`` inside Rash evaluates Bash code in that child session.
