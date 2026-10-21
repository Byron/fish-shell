//! Platform-independent redirection modes from Fish syntax.

#[derive(Clone, Copy, Debug, PartialEq, Hash)]
pub enum RedirectionMode {
    Overwrite, // normal redirection: > file.txt
    Append,    // appending redirection: >> file.txt
    Input,     // input redirection: < file.txt
    TryInput,  // try-input redirection: <? file.txt
    Fd,        // fd redirection: 2>&1
    NoClob,    // noclobber redirection: >? file.txt
}
