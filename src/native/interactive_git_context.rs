//! Literal Git argument contexts for native completion.
//!
//! This module never evaluates words or runs Git. Unknown option arity declines
//! semantic completion instead of treating an option value as a revision.

type Choices = &'static [(&'static str, &'static str)];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RefFilter {
    All,
    Branches,
    LocalBranches,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Target {
    Subcommand,
    Refs(RefFilter),
    Options(Choices),
    Files,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Context {
    /// Canonical pairs for repository routing only; never user configuration.
    pub globals: Vec<String>,
    pub target: Target,
    pub files: bool,
    /// Literal prefix retained when completing an attached revision argument.
    pub prefix: String,
}

fn result(
    globals: Vec<String>,
    target: Target,
    files: bool,
    prefix: String,
    config_override: bool,
) -> Context {
    if config_override && matches!(target, Target::Refs(_)) {
        Context {
            globals,
            target: Target::None,
            files: false,
            prefix: String::new(),
        }
    } else {
        Context {
            globals,
            target,
            files,
            prefix,
        }
    }
}

/// `args` contains complete words after Git's executable; `current` is the word
/// being completed. Callers must decline words requiring shell expansion.
pub(super) fn context(args: &[String], current: &str) -> Option<Context> {
    let mut globals = Vec::new();
    let mut index = 0;
    let mut config_override = false;
    while let Some(arg) = args.get(index) {
        if !arg.starts_with('-') {
            break;
        }
        if let Some((name, attached)) = global_value(arg) {
            let route = matches!(name, "-C" | "--git-dir" | "--work-tree" | "--namespace");
            let value = if let Some(value) = attached {
                value
            } else {
                index += 1;
                let Some(value) = args.get(index) else {
                    let files = matches!(name, "-C" | "--git-dir" | "--work-tree");
                    return Some(result(
                        globals,
                        if files { Target::Files } else { Target::None },
                        files,
                        String::new(),
                        false,
                    ));
                };
                value
            };
            if route {
                globals.extend([name.to_owned(), value.to_owned()]);
            } else {
                // Do not execute typed -c or --config-env settings in a query.
                config_override = true;
            }
        } else if arg == "--bare" {
            globals.push(arg.clone());
        } else if matches!(
            arg.as_str(),
            "--no-pager"
                | "--paginate"
                | "-P"
                | "-p"
                | "--no-replace-objects"
                | "--literal-pathspecs"
                | "--glob-pathspecs"
                | "--noglob-pathspecs"
                | "--icase-pathspecs"
        ) {
            // These do not change ref discovery; the provider disables paging.
        } else if matches!(
            arg.as_str(),
            "--help" | "-h" | "--version" | "-v" | "--html-path" | "--man-path" | "--info-path"
        ) {
            return Some(result(globals, Target::None, false, String::new(), false));
        } else {
            return None;
        }
        index += 1;
    }
    let Some(command) = args.get(index).map(String::as_str) else {
        let target = if global_value(current).is_some_and(|(_, value)| value.is_some()) {
            // Attached routing paths require replacing only their value. The
            // ordinary path completer currently replaces a whole word.
            Target::None
        } else if current.starts_with('-') {
            Target::Options(GLOBAL_OPTIONS)
        } else {
            Target::Subcommand
        };
        return Some(result(globals, target, false, String::new(), false));
    };
    let options = choices(command)?;
    let mut state = State::default();
    index += 1;
    while let Some(arg) = args.get(index) {
        if !state.separator && arg == "--" {
            state.separator = true;
        } else if !state.separator && arg.starts_with('-') && arg != "-" {
            let (kind, attached, _) = option(command, arg)?;
            state.apply(kind);
            if let Kind::Value(argument) = kind {
                if attached.is_none() {
                    index += 1;
                    if args.get(index).is_none() {
                        let (target, files) = argument.target();
                        return Some(result(
                            globals,
                            target,
                            files,
                            String::new(),
                            config_override,
                        ));
                    }
                }
            }
        } else {
            state.positionals += 1;
        }
        index += 1;
    }
    if state.stop {
        return Some(result(globals, Target::None, false, String::new(), false));
    }
    if !state.separator && current.starts_with('-') {
        if let Some((kind, Some(_), prefix)) = option(command, current) {
            let (target, files) = match kind {
                Kind::Value(Argument::Refs) => (Target::Refs(RefFilter::All), false),
                // Do not offer revisions, files or flags inside another
                // option's attached value, including a new branch's name.
                _ => (Target::None, false),
            };
            return Some(result(
                globals,
                target,
                files,
                prefix.to_owned(),
                config_override,
            ));
        }
        return Some(result(
            globals,
            Target::Options(options),
            false,
            String::new(),
            false,
        ));
    }
    let (target, files) = state.target(command);
    Some(result(
        globals,
        target,
        files,
        String::new(),
        config_override,
    ))
}

/// Normalize routing options so the query provider can validate a small argv
/// vocabulary. Empty `-C` values and repeated relative `-C` keep Git's ordering.
fn global_value(arg: &str) -> Option<(&'static str, Option<&str>)> {
    for name in [
        "-C",
        "-c",
        "--git-dir",
        "--work-tree",
        "--namespace",
        "--config-env",
    ] {
        if arg == name {
            return Some((name, None));
        }
        if let Some(value) = arg.strip_prefix(name) {
            if name.len() == 2 && !value.is_empty() {
                return Some((name, Some(value)));
            }
            if let Some(value) = value.strip_prefix('=') {
                return Some((name, Some(value)));
            }
        }
    }
    None
}

#[derive(Clone, Copy)]
enum Argument {
    None,
    Files,
    Refs,
    NewBranch,
    Orphan,
    PathspecFile,
}

impl Argument {
    fn target(self) -> (Target, bool) {
        match self {
            Self::Refs => (Target::Refs(RefFilter::All), false),
            Self::Files | Self::PathspecFile => (Target::Files, true),
            _ => (Target::None, false),
        }
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Flag,
    Optional,
    Value(Argument),
    NoPaths,
    Detach,
    Track,
    Root,
    Stop,
}

#[derive(Default)]
struct State {
    separator: bool,
    positionals: usize,
    no_paths: bool,
    create: bool,
    orphan: bool,
    detach: bool,
    track: bool,
    root: bool,
    stop: bool,
}

impl State {
    fn apply(&mut self, kind: Kind) {
        match kind {
            Kind::NoPaths | Kind::Value(Argument::PathspecFile) => self.no_paths = true,
            Kind::Value(Argument::NewBranch) => self.create = true,
            Kind::Value(Argument::Orphan) => {
                self.create = true;
                self.orphan = true;
            }
            Kind::Detach => self.detach = true,
            Kind::Track => self.track = true,
            Kind::Root => self.root = true,
            Kind::Stop => self.stop = true,
            _ => {}
        }
    }

    fn target(&self, command: &str) -> (Target, bool) {
        let all = Target::Refs(RefFilter::All);
        match command {
            "reset" => {
                if self.separator || self.positionals > 0 {
                    if self.no_paths {
                        (Target::None, false)
                    } else {
                        (Target::Files, true)
                    }
                } else {
                    (all, !self.no_paths)
                }
            }
            "checkout" => {
                if self.separator || self.positionals > 0 {
                    if self.create || self.detach || self.no_paths {
                        (Target::None, false)
                    } else {
                        (Target::Files, true)
                    }
                } else {
                    (
                        all,
                        !self.create && !self.detach && !self.track && !self.no_paths,
                    )
                }
            }
            "switch" if self.positionals == 0 && !self.orphan => {
                let filter = if self.create || self.detach {
                    RefFilter::All
                } else if self.track {
                    RefFilter::Branches
                } else {
                    RefFilter::LocalBranches
                };
                (Target::Refs(filter), false)
            }
            "switch" => (Target::None, false),
            "merge" => (all, false),
            "rebase" if self.positionals < if self.root { 1 } else { 2 } => (all, false),
            "rebase" => (Target::None, false),
            "restore" => (Target::Files, true),
            "diff" | "log" | "show" if !self.separator => (all, true),
            "diff" | "log" | "show" => (Target::Files, true),
            _ => (Target::None, false),
        }
    }
}

fn option<'a>(command: &str, arg: &'a str) -> Option<(Kind, Option<&'a str>, &'a str)> {
    if let Some(kind) = option_kind(command, arg) {
        return Some((kind, None, ""));
    }
    if arg.starts_with('-') && !arg.starts_with("--") && arg.len() > 2 {
        let kind = option_kind(command, arg.get(..2)?)?;
        if matches!(kind, Kind::Value(_) | Kind::Optional) {
            return Some((kind, Some(&arg[2..]), &arg[..2]));
        }
    } else if let Some((name, value)) = arg.split_once('=') {
        let kind = option_kind(command, name)?;
        if matches!(kind, Kind::Value(_) | Kind::Optional | Kind::Track) {
            return Some((kind, Some(value), &arg[..name.len() + 1]));
        }
    }
    None
}

fn option_kind(command: &str, name: &str) -> Option<Kind> {
    use Argument as A;
    use Kind as K;
    if matches!(name, "--help" | "-h") {
        return Some(K::Stop);
    }
    if matches!(command, "reset" | "checkout" | "restore") && name == "--pathspec-from-file" {
        return Some(K::Value(A::PathspecFile));
    }
    let kind = match (command, name) {
        ("reset", "--hard" | "--soft" | "--merge" | "--keep") => K::NoPaths,
        ("checkout", "-b" | "-B") | ("switch", "-c" | "-C" | "--create" | "--force-create") => {
            K::Value(A::NewBranch)
        }
        ("checkout" | "switch", "--orphan") => K::Value(A::Orphan),
        ("checkout" | "switch", "--detach" | "-d") => K::Detach,
        ("checkout" | "switch", "--track" | "-t") => K::Track,
        ("checkout" | "switch" | "restore", "--conflict") => K::Value(A::None),
        ("checkout" | "switch", "--recurse-submodules") => K::Optional,
        ("restore", "--source" | "-s") | ("rebase", "--onto") => K::Value(A::Refs),
        ("merge", "-F" | "--file") => K::Value(A::Files),
        ("merge", "-m" | "--message" | "--into-name" | "--cleanup") => K::Value(A::None),
        ("merge" | "rebase", "-s" | "--strategy" | "-X" | "--strategy-option") => K::Value(A::None),
        ("merge" | "rebase", "-S" | "--gpg-sign") => K::Optional,
        ("merge", "--log") | ("rebase", "--rebase-merges") => K::Optional,
        ("merge" | "rebase", "--abort" | "--continue" | "--quit") => K::Stop,
        ("rebase", "--skip" | "--edit-todo" | "--show-current-patch") => K::Stop,
        ("rebase", "--exec" | "-x" | "--empty" | "--whitespace") => K::Value(A::None),
        ("rebase", "--root") => K::Root,
        ("diff" | "log" | "show", "--output") => K::Value(A::Files),
        (
            "diff" | "log" | "show",
            "-S" | "-G" | "-O" | "--diff-filter" | "--format" | "--pretty" | "--encoding"
            | "--since" | "--until" | "--after" | "--before" | "--author" | "--committer"
            | "--grep" | "--max-count" | "-n" | "--skip",
        ) => K::Value(A::None),
        (
            "diff" | "log" | "show",
            "--branches" | "--tags" | "--remotes" | "--stat" | "--color" | "--decorate",
        ) => K::Optional,
        _ => {
            let flags: &[&str] = match command {
                "reset" => &[
                    "--mixed",
                    "-p",
                    "--patch",
                    "-q",
                    "--quiet",
                    "-N",
                    "--intent-to-add",
                    "--refresh",
                    "--no-refresh",
                    "--pathspec-file-nul",
                    "--recurse-submodules",
                    "--no-recurse-submodules",
                ],
                "checkout" => &[
                    "-f",
                    "--force",
                    "-q",
                    "--quiet",
                    "-m",
                    "--merge",
                    "-p",
                    "--patch",
                    "--ours",
                    "--theirs",
                    "--guess",
                    "--no-guess",
                    "--no-track",
                    "--overlay",
                    "--no-overlay",
                    "--progress",
                    "--no-progress",
                    "--no-recurse-submodules",
                    "--ignore-skip-worktree-bits",
                    "--ignore-other-worktrees",
                    "--pathspec-file-nul",
                ],
                "switch" => &[
                    "-f",
                    "--force",
                    "--discard-changes",
                    "-q",
                    "--quiet",
                    "-m",
                    "--merge",
                    "--guess",
                    "--no-guess",
                    "--no-track",
                    "--progress",
                    "--no-progress",
                    "--no-recurse-submodules",
                    "--ignore-other-worktrees",
                ],
                "merge" => &[
                    "--commit",
                    "--no-commit",
                    "-e",
                    "--edit",
                    "--no-edit",
                    "--ff",
                    "--no-ff",
                    "--ff-only",
                    "--no-log",
                    "--signoff",
                    "--no-signoff",
                    "--stat",
                    "-n",
                    "--no-stat",
                    "--squash",
                    "--no-squash",
                    "--verify-signatures",
                    "--no-verify-signatures",
                    "-q",
                    "--quiet",
                    "-v",
                    "--verbose",
                    "--progress",
                    "--no-progress",
                    "--allow-unrelated-histories",
                    "--rerere-autoupdate",
                    "--no-rerere-autoupdate",
                    "--autostash",
                    "--no-autostash",
                ],
                "rebase" => &[
                    "-i",
                    "--interactive",
                    "-m",
                    "--merge",
                    "-q",
                    "--quiet",
                    "-v",
                    "--verbose",
                    "--stat",
                    "-n",
                    "--no-stat",
                    "--verify",
                    "--no-verify",
                    "-f",
                    "--force-rebase",
                    "--keep-empty",
                    "--no-keep-empty",
                    "--keep-base",
                    "--autosquash",
                    "--no-autosquash",
                    "--autostash",
                    "--no-autostash",
                    "--no-ff",
                    "--no-rebase-merges",
                    "--update-refs",
                    "--no-update-refs",
                    "--fork-point",
                    "--no-fork-point",
                    "--committer-date-is-author-date",
                    "--ignore-date",
                    "--reset-author-date",
                    "--reapply-cherry-picks",
                ],
                "restore" => &[
                    "-p",
                    "--patch",
                    "-W",
                    "--worktree",
                    "-S",
                    "--staged",
                    "--ours",
                    "--theirs",
                    "-m",
                    "--merge",
                    "--ignore-unmerged",
                    "--ignore-skip-worktree-bits",
                    "--overlay",
                    "--no-overlay",
                    "-q",
                    "--quiet",
                    "--progress",
                    "--no-progress",
                    "--pathspec-file-nul",
                ],
                "diff" | "log" | "show" => &[
                    "--all",
                    "--cached",
                    "--staged",
                    "--patch",
                    "-p",
                    "--no-patch",
                    "-s",
                    "--name-only",
                    "--name-status",
                    "--oneline",
                    "--graph",
                    "--no-decorate",
                    "--no-color",
                    "--relative",
                    "--reverse",
                    "--first-parent",
                    "--merges",
                    "--no-merges",
                    "--follow",
                    "--check",
                    "--quiet",
                    "--exit-code",
                    "--binary",
                    "--full-index",
                    "--no-ext-diff",
                    "--no-textconv",
                ],
                _ => return None,
            };
            return flags.contains(&name).then_some(K::Flag);
        }
    };
    Some(kind)
}

const GLOBAL_OPTIONS: Choices = &[
    ("-C", "Run in another directory"),
    ("--git-dir", "Use a Git directory"),
    ("--work-tree", "Use a working tree"),
    ("--bare", "Use a bare repository"),
    ("--no-pager", "Disable paging"),
    ("--help", "Show help"),
    ("--version", "Show version"),
];
const RESET_OPTIONS: Choices = &[
    ("--hard", "Reset index and working tree"),
    ("--soft", "Move HEAD only"),
    ("--mixed", "Reset HEAD and index"),
    ("--merge", "Reset while preserving local changes"),
    ("--keep", "Keep local working tree changes"),
    ("--patch", "Select hunks to reset"),
    ("--quiet", "Suppress feedback"),
    ("--pathspec-from-file", "Read paths from a file"),
];
const CHECKOUT_OPTIONS: Choices = &[
    ("-b", "Create a branch"),
    ("-B", "Create or reset a branch"),
    ("--detach", "Inspect a commit"),
    ("--track", "Track a branch"),
    ("--force", "Discard conflicting local changes"),
    ("--patch", "Select changes interactively"),
    ("--ours", "Restore our version"),
    ("--theirs", "Restore their version"),
    ("--orphan", "Create an orphan branch"),
];
const SWITCH_OPTIONS: Choices = &[
    ("--create", "Create a branch"),
    ("--force-create", "Create or reset a branch"),
    ("--detach", "Inspect a commit"),
    ("--track", "Track a branch"),
    ("--discard-changes", "Discard conflicting local changes"),
    ("--merge", "Merge local changes"),
    ("--orphan", "Create an orphan branch"),
];
const MERGE_OPTIONS: Choices = &[
    ("--ff-only", "Require a fast-forward"),
    ("--no-ff", "Always create a merge commit"),
    ("--squash", "Prepare a single combined change"),
    ("--no-commit", "Stop before committing"),
    ("--abort", "Abort a merge"),
    ("--continue", "Conclude a merge"),
    ("--strategy", "Select a merge strategy"),
    ("--message", "Supply the merge message"),
];
const REBASE_OPTIONS: Choices = &[
    ("--interactive", "Edit the commit sequence"),
    ("--onto", "Rebase onto a different base"),
    ("--root", "Rebase from the root"),
    ("--continue", "Continue a rebase"),
    ("--abort", "Abort a rebase"),
    ("--skip", "Skip the current commit"),
    ("--autosquash", "Reorder fixup commits"),
    ("--autostash", "Temporarily stash local changes"),
    ("--exec", "Run a command after each commit"),
];
const RESTORE_OPTIONS: Choices = &[
    ("--source", "Restore from a tree"),
    ("--staged", "Restore the index"),
    ("--worktree", "Restore the working tree"),
    ("--patch", "Select changes interactively"),
    ("--ours", "Restore our version"),
    ("--theirs", "Restore their version"),
    ("--pathspec-from-file", "Read paths from a file"),
];
const REVISION_OPTIONS: Choices = &[
    ("--all", "Include all refs"),
    ("--oneline", "Show one line per commit"),
    ("--stat", "Show a change summary"),
    ("--patch", "Show changes"),
    ("--name-only", "Show changed names"),
    ("--name-status", "Show changed names and status"),
    ("--no-color", "Disable colors"),
];
const DIFF_OPTIONS: Choices = &[
    ("--cached", "Compare staged changes"),
    ("--stat", "Show a change summary"),
    ("--patch", "Show changes"),
    ("--name-only", "Show changed names"),
    ("--name-status", "Show changed names and status"),
    ("--no-color", "Disable colors"),
];

fn choices(command: &str) -> Option<Choices> {
    Some(match command {
        "reset" => RESET_OPTIONS,
        "checkout" => CHECKOUT_OPTIONS,
        "switch" => SWITCH_OPTIONS,
        "merge" => MERGE_OPTIONS,
        "rebase" => REBASE_OPTIONS,
        "restore" => RESTORE_OPTIONS,
        "diff" => DIFF_OPTIONS,
        "log" | "show" => REVISION_OPTIONS,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(args: &[&str], current: &str) -> Context {
        context(
            &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
            current,
        )
        .unwrap()
    }

    #[test]
    fn reset_and_checkout_keep_revision_and_path_positions_separate() {
        for command in ["reset", "checkout"] {
            let first = get(&[command], "fea");
            assert_eq!(first.target, Target::Refs(RefFilter::All));
            assert!(first.files);
            for args in [
                vec![command, "HEAD"],
                vec![command, "--"],
                vec![command, "HEAD", "--"],
            ] {
                let next = get(&args, "src");
                assert_eq!(next.target, Target::Files, "{args:?}");
                assert!(next.files);
            }
        }
        for mode in ["--hard", "--soft", "--merge", "--keep"] {
            let first = get(&["reset", mode], "fea");
            assert_eq!(first.target, Target::Refs(RefFilter::All));
            assert!(!first.files);
            assert_eq!(get(&["reset", mode, "HEAD"], "").target, Target::None);
        }
    }

    #[test]
    fn branch_creation_names_and_switch_targets_do_not_receive_wrong_refs() {
        assert_eq!(
            get(&["switch"], "").target,
            Target::Refs(RefFilter::LocalBranches)
        );
        assert_eq!(
            get(&["switch", "--track"], "").target,
            Target::Refs(RefFilter::Branches)
        );
        for args in [
            vec!["switch", "--detach"],
            vec!["switch", "-c", "new"],
            vec!["checkout", "-b", "new"],
        ] {
            let value = get(&args, "base");
            assert_eq!(value.target, Target::Refs(RefFilter::All), "{args:?}");
            assert!(!value.files);
        }
        for args in [
            vec!["switch", "-c"],
            vec!["switch", "--orphan"],
            vec!["checkout", "-B"],
            vec!["checkout", "--orphan"],
        ] {
            assert_eq!(get(&args, "new").target, Target::None);
        }
        assert_eq!(get(&["switch", "--orphan", "new"], "").target, Target::None);
        assert_eq!(get(&["switch", "main"], "").target, Target::None);
        assert_eq!(get(&["switch"], "--create=new").target, Target::None);
        assert_eq!(get(&["checkout"], "-bnew").target, Target::None);
    }

    #[test]
    fn ref_option_values_preserve_attached_prefixes_and_ignore_option_values() {
        for (args, current, prefix) in [
            (vec!["rebase", "--onto"], "topic", ""),
            (vec!["rebase"], "--onto=topic", "--onto="),
            (vec!["restore", "--source"], "topic", ""),
            (vec!["restore"], "--source=topic", "--source="),
            (vec!["restore"], "-stopic", "-s"),
        ] {
            let value = get(&args, current);
            assert_eq!(value.target, Target::Refs(RefFilter::All));
            assert_eq!(value.prefix, prefix);
            assert!(!value.files);
        }
        for args in [
            vec!["merge", "-m"],
            vec!["merge", "--strategy"],
            vec!["rebase", "--exec"],
            vec!["rebase", "-X"],
            vec!["log", "--author"],
        ] {
            assert_eq!(get(&args, "main").target, Target::None, "{args:?}");
        }
        assert_eq!(
            get(&["rebase", "--exec", "echo main"], "base").target,
            Target::Refs(RefFilter::All)
        );
        assert_eq!(
            get(&["reset", "--pathspec-from-file"], "paths").target,
            Target::Files
        );
        assert_eq!(
            get(&["reset"], "--pathspec-from-file=paths").target,
            Target::None
        );
    }

    #[test]
    fn separators_and_operation_modes_follow_each_commands_grammar() {
        for command in ["merge", "rebase", "switch"] {
            assert!(matches!(
                get(&[command, "--"], "topic").target,
                Target::Refs(_)
            ));
            assert!(!get(&[command, "--"], "topic").files);
        }
        assert_eq!(
            get(&["merge", "one", "two"], "three").target,
            Target::Refs(RefFilter::All)
        );
        assert_eq!(get(&["rebase", "one", "two"], "three").target, Target::None);
        assert_eq!(
            get(&["rebase", "--root", "one"], "two").target,
            Target::None
        );
        for args in [
            vec!["merge", "--abort"],
            vec!["rebase", "--continue"],
            vec!["rebase", "--skip"],
        ] {
            assert_eq!(get(&args, "").target, Target::None);
        }
        for command in ["diff", "log", "show"] {
            assert_eq!(get(&[command], "main").target, Target::Refs(RefFilter::All));
            assert_eq!(get(&[command, "--"], "src").target, Target::Files);
        }
    }

    #[test]
    fn repository_routing_is_literal_ordered_and_configuration_is_never_forwarded() {
        let value = get(
            &[
                "-C",
                "one space",
                "-Ctwo",
                "-C",
                "",
                "--git-dir=odd$'name/.git",
                "--work-tree",
                "tree",
                "--namespace=ns",
                "--bare",
                "reset",
            ],
            "",
        );
        assert_eq!(
            value.globals,
            [
                "-C",
                "one space",
                "-C",
                "two",
                "-C",
                "",
                "--git-dir",
                "odd$'name/.git",
                "--work-tree",
                "tree",
                "--namespace",
                "ns",
                "--bare"
            ]
        );
        for args in [
            vec!["-c", "core.pager=anything", "reset"],
            vec!["-ccore.pager=anything", "reset"],
            vec!["--config-env=core.pager=ENV", "reset"],
        ] {
            let value = get(&args, "topic");
            assert_eq!(value.target, Target::None, "{args:?}");
            assert!(value.globals.is_empty());
        }
        assert_eq!(get(&["-C"], "path").target, Target::Files);
        assert_eq!(get(&["--git-dir"], "path").target, Target::Files);
        assert_eq!(get(&["-c"], "key=value").target, Target::None);
        assert_eq!(get(&["-C", "one"], "re").target, Target::Subcommand);
    }

    #[test]
    fn unknown_option_arity_declines_and_known_commands_get_their_own_flags() {
        for args in [
            vec!["--unknown", "value", "reset"],
            vec!["reset", "--unknown"],
            vec!["rebase", "-iq"],
            vec!["not-a-supported-command"],
        ] {
            let args: Vec<_> = args.into_iter().map(str::to_owned).collect();
            assert!(context(&args, "topic").is_none(), "{args:?}");
        }
        let Target::Options(options) = get(&["reset"], "--ha").target else {
            panic!()
        };
        assert!(options.iter().any(|(flag, _)| *flag == "--hard"));
        assert!(!options.iter().any(|(flag, _)| *flag == "--no-pager"));
    }
}
