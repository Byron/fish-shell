//! Read-only history queries used by the shipped prompts and completions.

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap};
use std::time::Instant;

use gix::bstr::ByteSlice;
use gix::prelude::ObjectIdExt;

pub(super) fn query(
    repository: &gix::Repository,
    command: &str,
    args: &[String],
) -> Result<Vec<u8>, i32> {
    let deadline = Instant::now() + super::QUERY_BUDGET;
    match command {
        "log" => log(repository, args, deadline),
        "rev-list" => count(repository, args, deadline),
        "show" => show(repository, args),
        "describe" => describe(repository, args, deadline),
        "reflog" if args.iter().all(|arg| arg == "--no-decorate") => {
            reflog(repository, false, deadline)
        }
        "stash" if args == ["list", "--format=%gd:%gs"] => reflog(repository, true, deadline),
        _ => Err(2),
    }
}

fn commit_id(repository: &gix::Repository, revision: &str) -> Result<gix::ObjectId, i32> {
    let object = repository
        .rev_parse_single(revision.as_bytes().as_bstr())
        .map_err(|_| 1)?
        .object()
        .map_err(|_| 1)?;
    object
        .peel_to_kind(gix::objs::Kind::Commit)
        .map(|object| object.id)
        .map_err(|_| 1)
}

fn check_budget(output: &[u8], deadline: Instant) -> Result<(), i32> {
    if output.len() > super::MAX_OUTPUT || Instant::now() >= deadline {
        Err(1)
    } else {
        Ok(())
    }
}

fn log(repository: &gix::Repository, args: &[String], deadline: Instant) -> Result<Vec<u8>, i32> {
    let mut max_count = 50;
    let mut all = false;
    let mut first_parent = false;
    let mut format = None;
    let mut grep = None;
    let mut revisions = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--all" => all = true,
            "--first-parent" => first_parent = true,
            "-1" => max_count = 1,
            "--no-show-signature" | "--extended-regexp" => (),
            _ if arg.starts_with("--max-count=") => {
                max_count = arg[12..].parse::<usize>().map_err(|_| 2)?.min(1000);
            }
            _ if arg.starts_with("--pretty=tformat:") => {
                let value = &arg[17..];
                if value != "%H\t%<(64,trunc)%s" && value != "%h\t%<(64,trunc)%s" {
                    return Err(2);
                }
                format = Some(value);
            }
            _ if arg.starts_with("--grep=") => {
                grep = Some(
                    pcre2::utf32::RegexBuilder::new()
                        .multi_line(true)
                        .build(arg[7..].chars().collect::<Vec<_>>())
                        .map_err(|_| 2)?,
                );
            }
            _ if !arg.starts_with('-') => revisions.push(commit_id(repository, arg)?),
            _ => return Err(2),
        }
    }
    if revisions.is_empty() {
        match commit_id(repository, "HEAD") {
            Ok(head) => revisions.push(head),
            Err(_) if all => (),
            Err(error) => return Err(error),
        }
    }
    if all {
        for reference in repository
            .references()
            .map_err(|_| 1)?
            .all()
            .map_err(|_| 1)?
        {
            check_budget(&[], deadline)?;
            let mut reference = reference.map_err(|_| 1)?;
            if let Ok(object) = reference.peel_to_kind(gix::objs::Kind::Commit) {
                revisions.push(object.id);
            }
        }
    }
    let mut walk =
        repository
            .rev_walk(revisions)
            .sorting(gix::revision::walk::Sorting::ByCommitTime(
                gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
            ));
    if first_parent {
        walk = walk.first_parent_only();
    }
    let mut output = Vec::new();
    let mut emitted = 0;
    for info in walk.all().map_err(|_| 1)? {
        if emitted == max_count {
            break;
        }
        check_budget(&output, deadline)?;
        let commit = info.map_err(|_| 1)?.object().map_err(|_| 1)?;
        let message = commit.message().map_err(|_| 1)?;
        if let Some(grep) = &grep {
            let text: Vec<_> = String::from_utf8_lossy(commit.message_raw().map_err(|_| 1)?)
                .chars()
                .collect();
            if !grep.is_match(&text).map_err(|_| 1)? {
                continue;
            }
        }
        if let Some(format) = format {
            let id = if format.starts_with("%H") {
                commit.id.to_string()
            } else {
                commit.short_id().map_err(|_| 1)?.to_string()
            };
            output.extend_from_slice(id.as_bytes());
            output.push(b'\t');
            output.extend_from_slice(truncated_subject(message.summary().as_ref()).as_bytes());
            output.push(b'\n');
        } else {
            // The SVN prompt consumes the final message line, not log metadata.
            output.extend_from_slice(commit.message_raw().map_err(|_| 1)?);
            if !output.ends_with(b"\n") {
                output.push(b'\n');
            }
        }
        emitted += 1;
    }
    check_budget(&output, deadline)?;
    Ok(output)
}

fn truncated_subject(subject: &[u8]) -> String {
    let subject = String::from_utf8_lossy(subject);
    let width = |character| fish_fallback::fish_wcwidth(character).unwrap_or(0);
    let total: usize = subject.chars().map(width).sum();
    if total <= 64 {
        return format!("{subject}{}", " ".repeat(64 - total));
    }
    let mut remaining = 62;
    let mut output = String::new();
    for character in subject.chars() {
        let width = width(character);
        if width > remaining {
            break;
        }
        output.push(character);
        remaining -= width;
    }
    output.push_str(&" ".repeat(remaining));
    output.push_str("..");
    output
}

fn count(repository: &gix::Repository, args: &[String], deadline: Instant) -> Result<Vec<u8>, i32> {
    if args.len() != 3
        || !args.iter().any(|arg| arg == "--count")
        || !args.iter().any(|arg| arg == "--left-right")
    {
        return Err(2);
    }
    let range = args.iter().find(|arg| !arg.starts_with("--")).ok_or(2)?;
    let (left, right) = range.split_once("...").ok_or(2)?;
    let left = commit_id(repository, left)?;
    let right = commit_id(repository, right)?;
    if left == right {
        return Ok(b"0\t0\n".to_vec());
    }
    let count = |from, hidden| {
        let mut count = 0usize;
        for info in repository
            .rev_walk([from])
            .with_hidden([hidden])
            .all()
            .map_err(|_| 1)?
        {
            check_budget(&[], deadline)?;
            info.map_err(|_| 1)?;
            count += 1;
        }
        Ok::<_, i32>(count)
    };
    Ok(format!("{}\t{}\n", count(left, right)?, count(right, left)?).into_bytes())
}

fn show(repository: &gix::Repository, args: &[String]) -> Result<Vec<u8>, i32> {
    let [revision] = args else { return Err(2) };
    if revision.starts_with('-') || !revision.contains(':') {
        return Err(2);
    }
    let object = repository
        .rev_parse_single(revision.as_bytes().as_bstr())
        .map_err(|_| 1)?
        .object()
        .map_err(|_| 1)?;
    let tree = object.try_into_tree().map_err(|_| 1)?;
    let mut output = format!("tree {revision}\n\n").into_bytes();
    for entry in tree.iter() {
        let entry = entry.map_err(|_| 1)?;
        output.extend_from_slice(entry.filename());
        if entry.mode().is_tree() {
            output.push(b'/');
        }
        output.push(b'\n');
        if output.len() > super::MAX_OUTPUT {
            return Err(1);
        }
    }
    Ok(output)
}

fn reflog(repository: &gix::Repository, stash: bool, deadline: Instant) -> Result<Vec<u8>, i32> {
    let reference;
    let platform = if stash {
        reference = match repository.find_reference("refs/stash") {
            Ok(reference) => reference,
            Err(_) => return Ok(Vec::new()),
        };
        reference.log_iter()
    } else {
        repository.head().map_err(|_| 1)?.log_iter()
    };
    // The convenience iterator fixes its line buffer at 4KiB; messages can be longer.
    let mut buffer = vec![0; super::MAX_OUTPUT];
    let Some(log) = platform
        .store
        .reflog_iter_rev(platform.name, &mut buffer)
        .map_err(|_| 1)?
    else {
        return Ok(Vec::new());
    };
    let mut output = Vec::new();
    for (index, entry) in log.enumerate() {
        check_budget(&output, deadline)?;
        let entry = entry.map_err(|_| 1)?;
        if stash {
            output.extend_from_slice(format!("stash@{{{index}}}:").as_bytes());
        } else {
            output.extend_from_slice(
                format!(
                    "{} HEAD@{{{index}}}: ",
                    entry.new_oid.attach(repository).shorten().map_err(|_| 1)?
                )
                .as_bytes(),
            );
        }
        output.extend_from_slice(&entry.message);
        output.push(b'\n');
    }
    check_budget(&output, deadline)?;
    Ok(output)
}

fn describe(
    repository: &gix::Repository,
    args: &[String],
    deadline: Instant,
) -> Result<Vec<u8>, i32> {
    let mut contains = false;
    let mut exact = false;
    let mut names = gix::commit::describe::SelectRef::AnnotatedTags;
    let mut revision = "HEAD";
    for arg in args {
        match arg.as_str() {
            "--contains" => contains = true,
            "--exact-match" => exact = true,
            "--tags" => names = gix::commit::describe::SelectRef::AllTags,
            "--all" => names = gix::commit::describe::SelectRef::AllRefs,
            _ if !arg.starts_with('-') => revision = arg,
            _ => return Err(2),
        }
    }
    let id = commit_id(repository, revision)?;
    let name = if contains {
        containing_name(
            repository,
            id,
            names == gix::commit::describe::SelectRef::AllRefs,
            deadline,
        )?
    } else {
        let commit = id
            .attach(repository)
            .object()
            .map_err(|_| 1)?
            .try_into_commit()
            .map_err(|_| 1)?;
        let mut describe = commit.describe().names(names);
        if exact {
            describe = describe.max_candidates(0);
        }
        describe.try_format().map_err(|_| 1)?.ok_or(1)?.to_string()
    };
    check_budget(name.as_bytes(), deadline)?;
    Ok(format!("{name}\n").into_bytes())
}

fn containing_name(
    repository: &gix::Repository,
    target: gix::ObjectId,
    all: bool,
    deadline: Instant,
) -> Result<String, i32> {
    let mut queue = BinaryHeap::new();
    let mut visited = BTreeSet::new();
    for reference in repository
        .references()
        .map_err(|_| 1)?
        .all()
        .map_err(|_| 1)?
    {
        check_budget(&[], deadline)?;
        let mut reference = reference.map_err(|_| 1)?;
        let full_name = reference.name().as_bstr().to_owned();
        let tag = full_name.starts_with(b"refs/tags/");
        if !all && !tag {
            continue;
        }
        let object = reference
            .follow_to_object()
            .map_err(|_| 1)?
            .object()
            .map_err(|_| 1)?;
        let annotated = object.kind == gix::objs::Kind::Tag;
        let mut date = if annotated {
            object
                .clone()
                .try_into_tag()
                .map_err(|_| 1)?
                .tagger()
                .map_err(|_| 1)?
                .map_or(0, |tagger| tagger.seconds())
        } else {
            i64::MAX
        };
        let Ok(commit) = object.peel_to_commit() else {
            continue;
        };
        if tag && !annotated {
            date = commit.time().map_err(|_| 1)?.seconds;
        }
        let name = full_name
            .strip_prefix(b"refs/heads/")
            .or_else(|| full_name.strip_prefix(b"refs/remotes/"))
            .or_else(|| full_name.strip_prefix(b"refs/tags/"))
            .unwrap_or(&full_name);
        let mut name = String::from_utf8_lossy(name).into_owned();
        if tag && all {
            name.insert_str(0, "tags/");
        }
        if annotated {
            name.push_str("^0");
        }
        queue.push(Reverse((!tag, date, 0usize, name, commit.id, 0usize)));
    }
    while let Some(Reverse((branch, date, distance, name, id, generation))) = queue.pop() {
        check_budget(&[], deadline)?;
        if !visited.insert(id) {
            continue;
        }
        if id == target {
            return Ok(if generation == 0 {
                name
            } else {
                format!("{}~{generation}", name.trim_end_matches("^0"))
            });
        }
        let commit = id
            .attach(repository)
            .object()
            .map_err(|_| 1)?
            .try_into_commit()
            .map_err(|_| 1)?;
        for (index, parent) in commit.parent_ids().enumerate() {
            let (parent_name, parent_generation, cost) = if index == 0 {
                (name.clone(), generation + 1, 1)
            } else {
                let base = name.trim_end_matches("^0");
                let parent_name = if generation == 0 {
                    format!("{base}^{}", index + 1)
                } else {
                    format!("{base}~{generation}^{}", index + 1)
                };
                (parent_name, 0, 65535)
            };
            queue.push(Reverse((
                branch,
                date,
                distance + cost,
                parent_name,
                parent.detach(),
                parent_generation,
            )));
        }
    }
    Err(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        repository: gix::Repository,
        tree: gix::ObjectId,
        _directory: super::super::test_support::TemporaryDirectory,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = super::super::test_support::TemporaryDirectory::new();
            let mut repository = gix::ThreadSafeRepository::init_opts(
                directory.path().join("repo"),
                gix::create::Kind::WithWorktree,
                Default::default(),
                gix::open::Options::isolated(),
            )
            .unwrap()
            .to_thread_local();
            repository.refs.write_reflog = gix::refs::store::WriteReflog::Disable;
            let blob = repository.write_blob(b"contents\n").unwrap().detach();
            let file = gix::objs::tree::Entry {
                mode: gix::objs::tree::EntryKind::Blob.into(),
                filename: "file".into(),
                oid: blob,
            };
            let nested = repository
                .write_object(&gix::objs::Tree {
                    entries: vec![file.clone()],
                })
                .unwrap()
                .detach();
            let tree = repository
                .write_object(&gix::objs::Tree {
                    entries: vec![
                        file,
                        gix::objs::tree::Entry {
                            mode: gix::objs::tree::EntryKind::Tree.into(),
                            filename: "nested".into(),
                            oid: nested,
                        },
                    ],
                })
                .unwrap()
                .detach();
            Self {
                _directory: directory,
                repository,
                tree,
            }
        }

        fn signature(seconds: i64) -> gix::actor::Signature {
            gix::actor::Signature {
                name: "Fish Test".into(),
                email: "fish@example.invalid".into(),
                time: gix::date::Time { seconds, offset: 0 },
            }
        }

        fn reference(&self, name: &str, id: gix::ObjectId) {
            self.repository
                .reference(
                    name,
                    id,
                    gix::refs::transaction::PreviousValue::Any,
                    "fixture",
                )
                .unwrap();
        }

        fn commit(
            &self,
            name: &str,
            parents: &[gix::ObjectId],
            message: &str,
            time: i64,
        ) -> gix::ObjectId {
            let id = self
                .repository
                .write_object(&gix::objs::Commit {
                    tree: self.tree,
                    parents: parents.iter().copied().collect(),
                    author: Self::signature(time),
                    committer: Self::signature(time),
                    encoding: None,
                    message: message.into(),
                    extra_headers: Vec::new(),
                })
                .unwrap()
                .detach();
            self.reference(name, id);
            id
        }

        fn query(&self, command: &str, args: &[&str]) -> String {
            String::from_utf8(
                query(
                    &self.repository,
                    command,
                    &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
                )
                .unwrap(),
            )
            .unwrap()
        }
    }

    #[test]
    fn divergent_history_counts_and_completion_messages() {
        let fixture = Fixture::new();
        let base = fixture.commit("HEAD", &[], "base\n", 1);
        let left = fixture.commit("refs/heads/left", &[base], "left\n", 2);
        let right = fixture.commit("HEAD", &[base], "right\n\nBody.\n", 3);
        assert_eq!(
            fixture.query("rev-list", &["--count", "--left-right", "left...HEAD"]),
            "1\t1\n"
        );
        assert_eq!(
            fixture.query("rev-list", &["--count", "--left-right", "HEAD...HEAD"]),
            "0\t0\n"
        );
        let log = fixture.query(
            "log",
            &[
                "--no-show-signature",
                "--pretty=tformat:%H\t%<(64,trunc)%s",
                "--all",
                "--max-count=2",
            ],
        );
        assert_eq!(
            log,
            format!("{right}\t{:<64}\n{left}\t{:<64}\n", "right", "left")
        );
        assert_eq!(
            fixture.query(
                "log",
                &[
                    "--pretty=tformat:%h\t%<(64,trunc)%s",
                    "--max-count=1",
                    "left"
                ]
            ),
            format!(
                "{}\t{:<64}\n",
                left.attach(&fixture.repository).shorten().unwrap(),
                "left"
            )
        );
        let merged = fixture.commit("HEAD", &[right, left], "merge\n", 4);
        assert_eq!(
            fixture.query("rev-list", &["--count", "--left-right", "left...HEAD"]),
            "0\t2\n"
        );
        assert_eq!(commit_id(&fixture.repository, "HEAD").unwrap(), merged);
    }

    #[test]
    fn svn_first_parent_grep_and_revision_tree() {
        let fixture = Fixture::new();
        let base = fixture.commit(
            "HEAD",
            &[],
            "import\n\ngit-svn-id: https://svn.example/trunk@1 id\n",
            1,
        );
        let side = fixture.commit(
            "refs/heads/side",
            &[base],
            "side\n\ngit-svn-id: https://svn.example/trunk@2 id\n",
            2,
        );
        fixture.commit("HEAD", &[base, side], "merge\n", 3);
        assert_eq!(
            fixture.query(
                "log",
                &[
                    "--first-parent",
                    "-1",
                    "--extended-regexp",
                    "--grep=^git-svn-id: (https://svn.example/trunk)"
                ]
            ),
            "import\n\ngit-svn-id: https://svn.example/trunk@1 id\n"
        );
        assert_eq!(
            fixture.query("show", &["HEAD:"]),
            "tree HEAD:\n\nfile\nnested/\n"
        );
        assert_eq!(
            fixture.query("show", &["HEAD:nested/"]),
            "tree HEAD:nested/\n\nfile\n"
        );
        assert_eq!(
            query(&fixture.repository, "show", &["HEAD:file".into()]),
            Err(1)
        );
    }

    #[test]
    fn tag_descriptions_and_containing_names() {
        let fixture = Fixture::new();
        let base = fixture.commit("HEAD", &[], "base\n", 1);
        let tip = fixture.commit("HEAD", &[base], "tip\n", 2);
        let tag = fixture
            .repository
            .write_object(&gix::objs::Tag {
                target: tip,
                target_kind: gix::objs::Kind::Commit,
                name: "v1".into(),
                tagger: Some(Fixture::signature(3)),
                message: "release\n".into(),
                signature: None,
            })
            .unwrap()
            .detach();
        fixture.reference("refs/tags/v1", tag);
        assert_eq!(fixture.query("describe", &["HEAD"]), "v1\n");
        assert_eq!(
            fixture.query("describe", &["--tags", "--exact-match", "HEAD"]),
            "v1\n"
        );
        assert_eq!(fixture.query("describe", &["--contains", "HEAD"]), "v1^0\n");
        assert_eq!(
            fixture.query("describe", &["--contains", &base.to_string()]),
            "v1~1\n"
        );
        assert_eq!(
            fixture.query("describe", &["--contains", "--all", &base.to_string()]),
            "tags/v1~1\n"
        );
        fixture.reference("refs/tags/light", tip);
        assert_eq!(
            fixture.query("describe", &["--contains", "HEAD"]),
            "light\n"
        );
        assert_eq!(
            fixture.query("describe", &["--contains", &base.to_string()]),
            "light~1\n"
        );
        fixture.commit("HEAD", &[tip], "next\n", 4);
        assert_eq!(
            query(
                &fixture.repository,
                "describe",
                &["--tags".into(), "--exact-match".into(), "HEAD".into()]
            ),
            Err(1)
        );
        assert!(fixture.query("describe", &["HEAD"]).starts_with("v1-1-g"));
    }

    #[test]
    fn invalid_history_arguments_are_distinct_from_missing_revisions() {
        let fixture = Fixture::new();
        fixture.commit("HEAD", &[], "base\n", 1);
        assert_eq!(
            query(&fixture.repository, "log", &["--pretty=tformat:%B".into()]),
            Err(2)
        );
        assert_eq!(
            query(
                &fixture.repository,
                "describe",
                &["--long".into(), "HEAD".into()]
            ),
            Err(2)
        );
        assert_eq!(
            query(&fixture.repository, "describe", &["missing".into()]),
            Err(1)
        );
    }

    #[test]
    fn reflog_and_stash_preserve_order_and_messages() {
        let fixture = Fixture::new();
        let base = fixture.commit("HEAD", &[], "base\n", 1);
        let tip = fixture.commit("HEAD", &[base], "tip\n", 2);
        let logs = fixture.repository.git_dir().join("logs");
        std::fs::create_dir_all(logs.join("refs")).unwrap();
        let log = format!(
            "{base} {base} Fish Test <fish@example.invalid> 1 +0000\tfirst\n{base} {tip} Fish Test <fish@example.invalid> 2 +0000\tsecond\n"
        );
        std::fs::write(logs.join("HEAD"), &log).unwrap();
        std::fs::write(logs.join("refs/stash"), log).unwrap();
        fixture.reference("refs/stash", tip);
        assert_eq!(
            fixture.query("stash", &["list", "--format=%gd:%gs"]),
            "stash@{0}:second\nstash@{1}:first\n"
        );
        assert_eq!(
            fixture.query("reflog", &["--no-decorate"]),
            format!(
                "{} HEAD@{{0}}: second\n{} HEAD@{{1}}: first\n",
                tip.attach(&fixture.repository).shorten().unwrap(),
                base.attach(&fixture.repository).shorten().unwrap()
            )
        );
        assert_eq!(
            query(&fixture.repository, "stash", &["drop".into()]),
            Err(2)
        );
    }

    #[test]
    fn subjects_truncate_display_columns() {
        assert_eq!(
            truncated_subject("界".repeat(40).as_bytes()),
            format!("{}..", "界".repeat(31))
        );
        assert_eq!(truncated_subject(b"short"), format!("{:<64}", "short"));
        assert_eq!(
            truncated_subject("é".repeat(65).as_bytes()),
            format!("{}..", "é".repeat(62))
        );
    }

    #[test]
    fn reflog_messages_can_exceed_the_gix_convenience_buffer() {
        let fixture = Fixture::new();
        let id = fixture.commit("HEAD", &[], "base\n", 1);
        fixture.reference("refs/stash", id);
        let logs = fixture.repository.git_dir().join("logs");
        std::fs::create_dir_all(logs.join("refs")).unwrap();
        let message = "x".repeat(5000);
        let log = format!("{id} {id} Fish Test <fish@example.invalid> 1 +0000\t{message}\n");
        std::fs::write(logs.join("HEAD"), &log).unwrap();
        std::fs::write(logs.join("refs/stash"), log).unwrap();
        assert_eq!(
            fixture.query("stash", &["list", "--format=%gd:%gs"]),
            format!("stash@{{0}}:{message}\n")
        );
        assert_eq!(
            fixture.query("reflog", &["--no-decorate"]),
            format!(
                "{} HEAD@{{0}}: {message}\n",
                id.attach(&fixture.repository).shorten().unwrap()
            )
        );
    }
}
