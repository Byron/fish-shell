# localization: tier1
function __fish_edit_command_buffer --argument-names language native_tmpdir
    set -l native false
    status is-native-windows; and set native true
    set -l tmpdir $native_tmpdir
    if not set -q tmpdir[1]
        set tmpdir (__fish_mktemp_relative -d fish)
        or return 1
    end
    set -l extension fish
    test "$language" = bash; and set extension sh
    set -l f $tmpdir/command-line.$extension
    printf '' >$f
    or return 1

    set -l editor (__fish_anyeditor)
    or return 1

    set -l indenter fish_indent
    if $native
        set -l sibling (path dirname -- (status fish-path))/fish_indent.exe
        test -f $sibling; and set indenter $sibling
    end
    set -l indented_lines
    if test "$language" = bash
        set indented_lines (commandline -b)
        commandline -b >$f
    else
        set indented_lines (commandline -b | $indenter --only-indent)
        string join -- \n $indented_lines >$f
    end
    set -l offset (commandline --cursor)
    # compute cursor line/column
    # string split emits explicit records, including trailing empty lines that
    # an ordinary command substitution can otherwise discard.
    set -l lines (commandline -b | string split \n)\n
    set -l line 1
    while set -q lines[1]; and test $offset -ge (string length -- "$lines[1]")
        set offset (math $offset - (string length -- "$lines[1]"))
        set line (math $line + 1)
        set -e lines[1]
    end
    set -l indent 0
    if test "$language" != bash
        set indent 1 + (string length -- "$indented_lines[$line]") - (string length -- "$lines[1]")
    end
    set -l col (math $offset + 1 + $indent)

    set -l editor_basename (string match -r '[^/]+$' -- $editor[1])
    if $native
        set editor_basename (path basename -- $editor[1] | string replace -r '(?i)\.exe$' '')
    end
    set -l wrapped_commands
    for wrap_target in (complete -- $editor_basename | string replace -rf '^complete [^/]+ --wraps (.+)$' '$1')
        set -l tmp
        string unescape -- $wrap_target | read -at tmp
        set -a wrapped_commands $tmp[1]
    end
    set -l found false
    set -l cursor_from_editor
    for editor_command in $editor_basename $wrapped_commands
        switch $editor_command
            case vi vim nvim
                if test $editor_command = vi && not set -l vi_version "$(vi --version 2>/dev/null)"
                    if printf %s $vi_version | grep -q BusyBox
                        break
                    end
                    set -a editor +{$line} $f
                    set found true
                    break
                end
                if $native
                    set cursor_from_editor $tmpdir/cursor-position
                    printf '' >$cursor_from_editor; or return
                else
                    set cursor_from_editor (__fish_mktemp_relative fish-edit_command_buffer)
                    or return
                end
                set -a editor +$line "+norm! $col|" $f \
                    '+au VimLeave * ++once call writefile([printf("%s %s %s", shellescape(bufname()), line("."), col("."))], "'$cursor_from_editor'")'
            case emacs emacsclient gedit
                set -a editor +$line:$col $f
            case kak
                if $native
                    set cursor_from_editor $tmpdir/cursor-position
                    printf '' >$cursor_from_editor; or return
                else
                    set cursor_from_editor (__fish_mktemp_relative fish-edit_command_buffer)
                    or return
                end
                set -a editor +$line:$col $f -e "
                        hook -always -once global ClientClose %val{client} %{
                            echo -to-file $cursor_from_editor -quoting shell \
                                %val{buffile} %val{cursor_line} %val{cursor_column}
                        }
                    "
            case nano
                set -a editor +$line,$col $f
            case joe ee
                set -a editor +$line $f
            case code code-oss
                set -a editor --goto $f:$line:$col --wait
            case subl
                set -a editor $f:$line:$col --wait
            case micro
                set -a editor $f +$line:$col
            case helix hx
                set -a editor $f:$line:$col
            case '*'
                continue
        end
        set found true
        break
    end
    if not $found
        set -a editor $f
    end

    $editor
    set -l editor_status $status

    set -l raw_lines (string split \n <$f)
    set -l unindented_lines $raw_lines
    set -l format_status 0
    if test "$language" != bash
        set unindented_lines (string join -- \n $raw_lines | $indenter --only-unindent)
        set format_status $status
    end

    set -l accepted false
    if test $editor_status -eq 0 -a $format_status -eq 0 -a -s $f
        # Set the command to the output of the edited command and move the cursor to the
        # end of the edited command.
        commandline -r -- $unindented_lines
        commandline -C 999999
        set accepted true
    else
        echo
        echo (_ "Ignoring the output of your editor since its exit status was non-zero")
        echo (_ "or the file was empty")
    end
    if $accepted; and set -q cursor_from_editor[1]
        set -l pos
        if $native
            read -at pos <$cursor_from_editor
        else
            eval set pos "$(command cat $cursor_from_editor)"
        end
        if set -q pos[1] && test $pos[1] = $f
            set -l line $pos[2]
            set -l indent (math (string length -- "$raw_lines[$line]") - (string length -- "$unindented_lines[$line]"))
            set -l column (math $pos[3] - $indent)
            if $native
                set -l offset (math $column - 1)
                set -l previous 1
                while test $previous -lt $line
                    set offset (math $offset + (string length -- "$unindented_lines[$previous]") + 1)
                    set previous (math $previous + 1)
                end
                commandline --cursor $offset
            else if not commandline --line $line 2>/dev/null
                commandline -f end-of-buffer
            else
                commandline --column $column 2>/dev/null || commandline -f end-of-line
            end
        end
    end
    if not $native
        set -q cursor_from_editor[1]; and command rm $cursor_from_editor
        command rm -r (path dirname $f)
    end
    # We've probably opened something that messed with the screen.
    # A repaint seems in order.
    commandline -f repaint
end
