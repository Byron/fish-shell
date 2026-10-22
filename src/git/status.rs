//! Read-only status queries used by the shipped prompts and completions.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice as _};
use gix::status::plumbing::index_as_worktree::{Change, Conflict, EntryStatus};
use gix::status::plumbing::index_as_worktree_with_renames::{Entry, Recorder};
use gix::submodule::config::Ignore;

#[derive(Clone)]
struct Options {
    untracked: gix::status::UntrackedFiles,
    ignored: bool,
    submodules: Option<Ignore>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            untracked: gix::status::UntrackedFiles::Collapsed,
            ignored: false,
            submodules: None,
        }
    }
}

struct FileStatus {
    xy: [u8; 2],
    submodule: [u8; 4],
    modes: [u32; 3],
    ids: [gix::ObjectId; 2],
    source: Option<Vec<u8>>,
    conflict:
        Option<Box<[Option<gix::status::plumbing::index_as_worktree::ConflictIndexEntry>; 3]>>,
}

impl FileStatus {
    fn new(hash: gix::hash::Kind) -> Self {
        Self {
            xy: *b"  ",
            submodule: *b"N...",
            modes: [0; 3],
            ids: [hash.null(); 2],
            source: None,
            conflict: None,
        }
    }

    fn from_index(entry: &gix::index::Entry) -> Self {
        Self {
            modes: [entry.mode.bits(); 3],
            ids: [entry.id; 2],
            submodule: if entry.mode == gix::index::entry::Mode::COMMIT {
                *b"S..."
            } else {
                *b"N..."
            },
            ..Self::new(entry.id.kind())
        }
    }
}

// Status needs clean/process normalization, but no external diff or textconv.
fn without_diff_drivers(repo: &gix::Repository) -> Result<gix::Repository, i32> {
    let mut repo = repo.clone();
    let mut config = repo.config_snapshot_mut();
    let sections: Vec<_> = config
        .sections_and_ids()
        .filter(|(section, _)| {
            section.header().name().eq_ignore_ascii_case(b"diff")
                && section.header().subsection_name().is_some()
        })
        .map(|(_, id)| id)
        .collect();
    for id in sections {
        config.remove_section_by_id(id);
    }
    config
        .append_config(
            ["diff.external=", "core.fsmonitor=false"],
            gix::config::Source::Api,
        )
        .map_err(|_| 128)?;
    config.commit().map_err(|_| 128)?;
    Ok(repo)
}

#[derive(Clone)]
struct Submodules {
    workdir: PathBuf,
    ignore: Option<Ignore>,
    default_ignore: Option<Ignore>,
    configured: BTreeMap<Vec<u8>, Ignore>,
    depth: usize,
}

impl gix::status::plumbing::index_as_worktree::traits::SubmoduleStatus for Submodules {
    type Output = [u8; 4];

    fn status(
        &mut self,
        entry: &gix::index::Entry,
        path: &BStr,
    ) -> gix::Result<Option<Self::Output>> {
        let ignore = self
            .ignore
            .or_else(|| self.configured.get(path.as_bytes()).copied())
            .or(self.default_ignore)
            .unwrap_or_default();
        if ignore == Ignore::All {
            return Ok(None);
        }
        let root = self.workdir.join(gix::path::from_bstr(path)?);
        if !root.join(".git").exists() {
            return Ok(None);
        }
        let repo = gix::open_opts(
            &root,
            gix::open::Options::isolated().config_overrides(["gitoxide.objects.noReplace=true"]),
        )?;
        let mut flags = *b"S...";
        if repo.head_id().is_ok_and(|id| id.detach() != entry.id) {
            flags[1] = b'C';
        }
        if ignore != Ignore::Dirty {
            if self.depth >= 8 {
                return Err(gix::Error::from_error(gix::error::message(
                    "submodule nesting is too deep",
                )));
            }
            let options = Options {
                untracked: if ignore == Ignore::Untracked {
                    gix::status::UntrackedFiles::None
                } else {
                    gix::status::UntrackedFiles::Collapsed
                },
                ..Options::default()
            };
            let records = collect(&repo, &options, self.depth + 1).map_err(|_| {
                gix::Error::from_error(gix::error::message("cannot read submodule status"))
            })?;
            for (_, record) in records {
                if record.xy == *b"??" {
                    flags[3] = b'U';
                } else if record.xy != *b"!!" {
                    flags[2] = b'M';
                }
            }
        }
        Ok((flags != *b"S...").then_some(flags))
    }
}

fn track_renames(repo: &gix::Repository) -> Result<gix::status::tree_index::TrackRenames, i32> {
    use gix::config::tree::Diff;
    use gix::diff::rename::Tracking;
    use gix::status::tree_index::TrackRenames;
    let snapshot = repo.config_snapshot();
    let config = &*snapshot;
    // The upstream Status keys currently point at merge.*, so read the correct keys explicitly.
    let renames = match config.boolean("status.renames") {
        Ok(None) => config.boolean("diff.renames"),
        value => value,
    };
    let tracking = Diff::RENAMES
        .try_into_renames(renames)
        .map_err(|_| 128)?
        .unwrap_or(Tracking::Renames);
    if tracking == Tracking::Disabled {
        return Ok(TrackRenames::Disabled);
    }
    let limit = match config.integer("status.renameLimit") {
        Ok(None) => config.integer("diff.renameLimit"),
        value => value,
    };
    Ok(TrackRenames::Given(gix::diff::Rewrites {
        copies: (tracking == Tracking::RenamesAndCopies).then(Default::default),
        limit: Diff::RENAME_LIMIT
            .try_into_usize(limit)
            .map_err(|_| 128)?
            .unwrap_or(gix::diff::Rewrites::default().limit),
        ..Default::default()
    }))
}

fn collect(
    repo: &gix::Repository,
    options: &Options,
    depth: usize,
) -> Result<Vec<(Vec<u8>, FileStatus)>, i32> {
    let repo = without_diff_drivers(repo)?;
    let index = repo.index_or_empty().map_err(|_| 128)?;
    let mut files = BTreeMap::new();
    let tree = repo.head_tree_id_or_empty().map_err(|_| 128)?;
    // Status covers the whole repository, even when the caller is in a subdirectory.
    let mut pathspec = repo
        .pathspec(
            false,
            None::<&str>,
            false,
            &index,
            gix::worktree::stack::state::attributes::Source::IdMapping,
        )
        .map_err(|_| 128)?;
    repo.tree_index_status(
        &tree,
        &index,
        Some(&mut pathspec),
        track_renames(&repo)?,
        |change, _, _| {
            use gix::diff::index::ChangeRef;
            let path = change.location().as_bytes().to_owned();
            let mut record = FileStatus::new(repo.object_hash());
            match change {
                ChangeRef::Addition { entry_mode, id, .. } => {
                    record.xy[0] = b'A';
                    record.modes[1..].fill(entry_mode.bits());
                    record.ids[1] = id.into_owned();
                }
                ChangeRef::Deletion { entry_mode, id, .. } => {
                    record.xy[0] = b'D';
                    record.modes[0] = entry_mode.bits();
                    record.ids[0] = id.into_owned();
                }
                ChangeRef::Modification {
                    previous_entry_mode,
                    previous_id,
                    entry_mode,
                    id,
                    ..
                } => {
                    record.xy[0] =
                        if previous_entry_mode.bits() & 0o170000 == entry_mode.bits() & 0o170000 {
                            b'M'
                        } else {
                            b'T'
                        };
                    record.modes = [
                        previous_entry_mode.bits(),
                        entry_mode.bits(),
                        entry_mode.bits(),
                    ];
                    record.ids = [previous_id.into_owned(), id.into_owned()];
                }
                ChangeRef::Rewrite {
                    source_location,
                    source_entry_mode,
                    source_id,
                    entry_mode,
                    id,
                    copy,
                    ..
                } => {
                    record.xy[0] = if copy { b'C' } else { b'R' };
                    record.source = Some(source_location.as_bytes().to_owned());
                    record.modes = [
                        source_entry_mode.bits(),
                        entry_mode.bits(),
                        entry_mode.bits(),
                    ];
                    record.ids = [source_id.into_owned(), id.into_owned()];
                }
            }
            if record.modes[..2].contains(&gix::index::entry::Mode::COMMIT.bits()) {
                record.submodule = *b"S...";
            }
            if options.submodules != Some(Ignore::All) || record.submodule[0] != b'S' {
                files.insert(path, record);
            }
            Ok(std::ops::ControlFlow::Continue(()))
        },
    )
    .map_err(|_| 128)?;

    let mut configured = BTreeMap::new();
    if options.submodules != Some(Ignore::All) {
        // modules() caches config overrides by .gitmodules mtime, even after
        // the repository config changes. Read worktree overrides afresh.
        let modules = match repo.open_modules_file().map_err(|_| 128)? {
            Some(modules) => Some(modules),
            None => repo
                .modules()
                .map_err(|_| 128)?
                .map(|modules| modules.into_owned_or_cloned()),
        };
        if let Some(modules) = modules {
            for name in modules.names() {
                if let Some(ignore) = modules.ignore(name).map_err(|_| 128)? {
                    configured.insert(modules.path(name).map_err(|_| 128)?.to_vec(), ignore);
                }
            }
        }
    }
    let global_ignore = repo
        .config_snapshot()
        .string("diff.ignoreSubmodules")
        .and_then(|value| Ignore::try_from(value.as_ref()).ok());
    let submodules = Submodules {
        workdir: repo.workdir().ok_or(128)?.to_owned(),
        ignore: options.submodules,
        default_ignore: global_ignore,
        configured,
        depth,
    };
    let mut walk = repo
        .dirwalk_options()
        .map_err(|_| 128)?
        .empty_patterns_match_prefix(false);
    walk = walk.emit_untracked(match options.untracked {
        gix::status::UntrackedFiles::Collapsed => gix::dir::walk::EmissionMode::CollapseDirectory,
        _ => gix::dir::walk::EmissionMode::Matching,
    });
    if options.ignored {
        walk = walk.emit_ignored(Some(gix::dir::walk::EmissionMode::CollapseDirectory));
    }
    let mut recorded = Recorder::default();
    repo.index_worktree_status(
        &index,
        [BString::from(":/")],
        &mut recorded,
        gix::status::plumbing::index_as_worktree::traits::FastEq,
        submodules,
        &mut gix::progress::Discard,
        &AtomicBool::new(false),
        gix::status::index_worktree::Options {
            dirwalk_options: (options.untracked != gix::status::UntrackedFiles::None
                || options.ignored)
                .then_some(walk),
            ..Default::default()
        },
    )
    .map_err(|_| 128)?;
    let mut untracked = Vec::new();
    for entry in recorded.records {
        match entry {
            Entry::Modification {
                entry,
                rela_path,
                status,
                ..
            } => {
                if matches!(status, EntryStatus::NeedsUpdate(_)) {
                    continue;
                }
                let record = files
                    .entry(rela_path.as_bytes().to_owned())
                    .or_insert_with(|| FileStatus::from_index(entry));
                match status {
                    EntryStatus::Change(Change::Removed) => {
                        record.xy[1] = b'D';
                        record.modes[2] = 0;
                    }
                    EntryStatus::Change(Change::Type { worktree_mode }) => {
                        record.xy[1] = b'T';
                        record.modes[2] = worktree_mode.bits();
                    }
                    EntryStatus::Change(Change::Modification {
                        executable_bit_changed,
                        ..
                    }) => {
                        record.xy[1] = b'M';
                        if executable_bit_changed {
                            record.modes[2] ^= 0o111;
                        }
                    }
                    EntryStatus::Change(Change::SubmoduleModification(flags)) => {
                        record.xy[1] = b'M';
                        record.submodule = flags;
                    }
                    EntryStatus::Conflict { summary, entries } => {
                        record.xy = *match summary {
                            Conflict::BothDeleted => b"DD",
                            Conflict::AddedByUs => b"AU",
                            Conflict::DeletedByThem => b"UD",
                            Conflict::AddedByThem => b"UA",
                            Conflict::DeletedByUs => b"DU",
                            Conflict::BothAdded => b"AA",
                            Conflict::BothModified => b"UU",
                        };
                        record.conflict = Some(entries);
                    }
                    EntryStatus::IntentToAdd => {
                        record.xy = *b" A";
                        record.modes[..2].fill(0);
                        record.ids.fill(repo.object_hash().null());
                    }
                    EntryStatus::NeedsUpdate(_) => unreachable!(),
                }
            }
            Entry::DirectoryContents { entry, .. } => {
                let xy = match entry.status {
                    gix::dir::entry::Status::Untracked
                        if options.untracked != gix::status::UntrackedFiles::None =>
                    {
                        *b"??"
                    }
                    gix::dir::entry::Status::Ignored(_) if options.ignored => *b"!!",
                    _ => continue,
                };
                let mut path = entry.rela_path.to_vec();
                if matches!(
                    entry.disk_kind,
                    Some(gix::dir::entry::Kind::Directory | gix::dir::entry::Kind::Repository)
                ) {
                    path.push(b'/');
                }
                let mut record = FileStatus::new(repo.object_hash());
                record.xy = xy;
                untracked.push((path, record));
            }
            Entry::Rewrite { .. } => unreachable!("index-worktree rename detection is disabled"),
        }
    }
    untracked.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files.into_iter().chain(untracked).collect())
}

fn relative_path(path: &[u8], prefix: &[u8]) -> Vec<u8> {
    let path: Vec<_> = path.split(|byte| *byte == b'/').collect();
    let prefix: Vec<_> = prefix
        .split(|byte| *byte == b'/')
        .filter(|part| !part.is_empty())
        .collect();
    let common = path.iter().zip(&prefix).take_while(|(a, b)| a == b).count();
    let mut result = b"../".repeat(prefix.len() - common);
    result.extend(path[common..].join(&b'/'));
    result
}

fn quote_path(path: &[u8], quote_high: bool) -> Vec<u8> {
    let needs_quotes = path.iter().any(|byte| {
        *byte < 0x20
            || *byte == b'"'
            || *byte == b'\\'
            || *byte == 0x7f
            || quote_high && *byte >= 0x80
    });
    if !needs_quotes {
        return path.to_vec();
    }
    let mut output = vec![b'"'];
    for byte in path {
        match byte {
            b'"' | b'\\' => output.extend([b'\\', *byte]),
            b'\n' => output.extend_from_slice(b"\\n"),
            b'\r' => output.extend_from_slice(b"\\r"),
            b'\t' => output.extend_from_slice(b"\\t"),
            0..=31 | 127..=255 if *byte < 0x80 || quote_high => {
                output.extend_from_slice(format!("\\{byte:03o}").as_bytes());
            }
            _ => output.push(*byte),
        }
    }
    output.push(b'"');
    output
}

pub(super) fn query(
    repo: &gix::Repository,
    cwd: &Path,
    command: &str,
    arguments: &[String],
) -> Result<Vec<u8>, i32> {
    let mut options = Options::default();
    if command == "status" {
        if let Some(value) = repo.config_snapshot().string("status.showUntrackedFiles") {
            options.untracked = gix::config::tree::Status::SHOW_UNTRACKED_FILES
                .try_into_show_untracked_files(value)
                .map_err(|_| 128)?;
        }
    }
    let mut porcelain_v2 = false;
    let mut nul = false;
    let mut cached = false;
    let mut others = false;
    let mut directory = false;
    let mut error_unmatch = false;
    let mut root_paths = false;
    let mut quiet = false;
    let mut paths = false;
    for argument in arguments {
        if paths {
            if argument != ":/" {
                return Err(129);
            }
            root_paths = true;
            continue;
        }
        match argument.as_str() {
            "--" => paths = true,
            "--porcelain" | "--porcelain=1" => {}
            "--porcelain=2" => porcelain_v2 = true,
            "-z" => nul = true,
            "-uno" | "--untracked-files=no" => {
                options.untracked = gix::status::UntrackedFiles::None
            }
            "-unormal" | "--untracked-files=normal" => {
                options.untracked = gix::status::UntrackedFiles::Collapsed
            }
            "-uall" | "--untracked-files=all" => {
                options.untracked = gix::status::UntrackedFiles::Files
            }
            "--ignored" => options.ignored = true,
            "--ignore-submodules=all" => options.submodules = Some(Ignore::All),
            "--ignore-submodules=dirty" => options.submodules = Some(Ignore::Dirty),
            "--ignore-submodules=untracked" => options.submodules = Some(Ignore::Untracked),
            "--ignore-submodules=none" => options.submodules = Some(Ignore::None),
            "--cached" | "-c" => cached = true,
            "--others" | "-o" => others = true,
            "--directory" => directory = true,
            "--error-unmatch" => error_unmatch = true,
            "--quiet" => quiet = true,
            "--exclude-standard" | "--no-empty-directory" | "--no-ext-diff" | "--exit-code" => {}
            "HEAD" if command == "diff-index" => {}
            _ => return Err(129),
        }
    }
    let root = gix::path::realpath(repo.workdir().ok_or(128)?).map_err(|_| 128)?;
    let cwd = gix::path::realpath(cwd).map_err(|_| 128)?;
    let prefix =
        gix::path::into_bstr(cwd.strip_prefix(&root).unwrap_or(Path::new(""))).map_err(|_| 128)?;
    let quote_high = repo
        .config_snapshot()
        .boolean("core.quotePath")
        .unwrap_or(true);
    if command == "ls-files" {
        let files: BTreeSet<Vec<u8>> = if others {
            let mut index = (**repo.index_or_empty().map_err(|_| 128)?).clone();
            for entry in index.entries_mut() {
                entry.flags.insert(gix::index::entry::Flags::UPTODATE);
            }
            let walk = repo
                .dirwalk_options()
                .map_err(|_| 128)?
                .empty_patterns_match_prefix(false)
                .emit_untracked(if directory {
                    gix::dir::walk::EmissionMode::CollapseDirectory
                } else {
                    gix::dir::walk::EmissionMode::Matching
                });
            let mut collected = gix::dir::walk::delegate::Collect::default();
            repo.dirwalk(
                &index,
                [BString::from(":/")],
                &AtomicBool::new(false),
                walk,
                &mut collected,
            )
            .map_err(|_| 128)?;
            collected
                .unorded_entries
                .into_iter()
                .filter_map(|(entry, _)| {
                    if entry.status != gix::dir::entry::Status::Untracked {
                        return None;
                    }
                    let mut path = entry.rela_path.to_vec();
                    if matches!(
                        entry.disk_kind,
                        Some(gix::dir::entry::Kind::Directory | gix::dir::entry::Kind::Repository)
                    ) {
                        path.push(b'/');
                    }
                    Some(path)
                })
                .collect()
        } else {
            let index = repo.index_or_empty().map_err(|_| 128)?;
            index
                .entries()
                .iter()
                .map(|entry| entry.path(&index).to_vec())
                .collect()
        };
        let mut output = Vec::new();
        for path in files {
            let path = if root_paths {
                path
            } else {
                relative_path(&path, &prefix)
            };
            if !root_paths && path.starts_with(b"../") {
                continue;
            }
            output.extend(if nul {
                path
            } else {
                quote_path(&path, quote_high)
            });
            output.push(if nul { 0 } else { b'\n' });
        }
        return if error_unmatch && output.is_empty() {
            Err(1)
        } else {
            Ok(output)
        };
    }
    if command == "diff" || command == "diff-index" {
        if !quiet {
            return Err(129);
        }
        options.untracked = gix::status::UntrackedFiles::None;
        let files = collect(repo, &options, 0)?;
        let changed = files.iter().any(|(_, record)| {
            if command == "diff-index" && !cached {
                record.xy != *b"  "
            } else {
                record.xy[usize::from(!cached)] != b' '
            }
        });
        return if changed { Err(1) } else { Ok(Vec::new()) };
    }
    if command != "status" {
        return Err(129);
    }
    let relative = porcelain_v2
        && !nul
        && repo
            .config_snapshot()
            .boolean("status.relativePaths")
            .unwrap_or(true);
    let mut files = collect(repo, &options, 0)?;
    files.sort_by_key(|(_, record)| match record.xy {
        [b'?', b'?'] => 1,
        [b'!', b'!'] => 2,
        _ => 0,
    });
    let mut output = Vec::new();
    for (path, record) in files {
        let path = if relative {
            relative_path(&path, &prefix)
        } else {
            path
        };
        let path = if nul {
            path
        } else {
            quote_path(&path, quote_high)
        };
        if porcelain_v2 {
            let xy = record.xy.map(|byte| if byte == b' ' { b'.' } else { byte });
            if record.xy == *b"??" || record.xy == *b"!!" {
                output.extend([record.xy[0], b' ']);
            } else if let Some(conflict) = &record.conflict {
                output.extend_from_slice(
                    format!(
                        "u {} {}",
                        String::from_utf8_lossy(&xy),
                        String::from_utf8_lossy(&record.submodule)
                    )
                    .as_bytes(),
                );
                for entry in conflict.iter() {
                    output.extend_from_slice(
                        format!(
                            " {:06o}",
                            entry.as_ref().map_or(0, |entry| entry.mode.bits())
                        )
                        .as_bytes(),
                    );
                }
                output.extend_from_slice(format!(" {:06o}", record.modes[2]).as_bytes());
                for entry in conflict.iter() {
                    output.extend_from_slice(
                        format!(
                            " {}",
                            entry
                                .as_ref()
                                .map_or(repo.object_hash().null(), |entry| entry.id)
                        )
                        .as_bytes(),
                    );
                }
                output.push(b' ');
            } else {
                output.extend_from_slice(
                    format!(
                        "{} {} {} {:06o} {:06o} {:06o} {} {} ",
                        if record.source.is_some() { '2' } else { '1' },
                        String::from_utf8_lossy(&xy),
                        String::from_utf8_lossy(&record.submodule),
                        record.modes[0],
                        record.modes[1],
                        record.modes[2],
                        record.ids[0],
                        record.ids[1]
                    )
                    .as_bytes(),
                );
                if record.source.is_some() {
                    // shortcut: inexact rename scores use 50%, compute scores if a caller needs them.
                    output.extend_from_slice(
                        format!(
                            "{}{} ",
                            record.xy[0] as char,
                            if record.ids[0] == record.ids[1] {
                                100
                            } else {
                                50
                            }
                        )
                        .as_bytes(),
                    );
                }
            }
        } else {
            output.extend(record.xy);
            output.push(b' ');
        }
        if let Some(source) = &record.source {
            let source = if relative {
                relative_path(source, &prefix)
            } else {
                source.clone()
            };
            let source = if nul {
                source
            } else {
                quote_path(&source, quote_high)
            };
            if nul || porcelain_v2 {
                output.extend(path);
                output.push(if nul { 0 } else { b'\t' });
                output.extend(source);
            } else {
                output.extend(source);
                output.extend_from_slice(b" -> ");
                output.extend(path);
            }
        } else {
            output.extend(path);
        }
        output.push(if nul { 0 } else { b'\n' });
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        root: PathBuf,
        repo: gix::Repository,
    }

    impl Fixture {
        fn new(files: &[(&str, &str)]) -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let root = loop {
                let root = std::env::temp_dir().join(format!(
                    "fish-gix-status-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                ));
                match std::fs::create_dir(&root) {
                    Ok(()) => break root,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("cannot create fixture: {error}"),
                }
            };
            let repo = gix::ThreadSafeRepository::init_opts(
                &root,
                gix::create::Kind::WithWorktree,
                Default::default(),
                gix::open::Options::isolated(),
            )
            .unwrap()
            .to_thread_local();
            let fixture = Self { root, repo };
            let tree = fixture.set_index(files);
            let signature = gix::actor::SignatureRef {
                name: "Fixture".into(),
                email: "fixture@example.com".into(),
                time: "1000000000 +0000",
            };
            fixture
                .repo
                .commit_as(
                    signature,
                    signature,
                    "HEAD",
                    "fixture",
                    tree,
                    None::<gix::ObjectId>,
                )
                .unwrap();
            for (name, content) in files {
                std::fs::write(fixture.root.join(name), content).unwrap();
            }
            fixture
        }

        fn set_index(&self, files: &[(&str, &str)]) -> gix::ObjectId {
            let mut entries: Vec<_> = files
                .iter()
                .map(|(name, content)| gix::objs::tree::Entry {
                    mode: gix::objs::tree::EntryKind::Blob.into(),
                    filename: (*name).into(),
                    oid: self.repo.write_blob(content).unwrap().detach(),
                })
                .collect();
            entries.sort();
            let tree = self
                .repo
                .write_object(&gix::objs::Tree { entries })
                .unwrap()
                .detach();
            self.repo
                .index_from_tree(&tree)
                .unwrap()
                .write(Default::default())
                .unwrap();
            tree
        }

        fn query(&self, command: &str, args: &[&str]) -> Result<Vec<u8>, i32> {
            query(
                &self.repo,
                &self.root,
                command,
                &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn status_and_quiet_diffs_distinguish_index_and_worktree_changes() {
        let fixture = Fixture::new(&[("tracked", "original\n")]);
        assert_eq!(
            fixture.query("status", &["--porcelain", "-z"]).unwrap(),
            b""
        );
        std::fs::write(fixture.root.join("tracked"), "worktree\n").unwrap();
        assert_eq!(
            fixture
                .query("status", &["--porcelain", "-z", "-uno"])
                .unwrap(),
            b" M tracked\0"
        );
        assert_eq!(
            fixture.query("diff", &["--no-ext-diff", "--quiet", "--exit-code"]),
            Err(1)
        );
        assert_eq!(
            fixture
                .query("diff-index", &["--cached", "--quiet", "HEAD", "--"])
                .unwrap(),
            b""
        );
        fixture.set_index(&[("tracked", "staged\n")]);
        assert_eq!(
            fixture
                .query("status", &["--porcelain", "-z", "-uno"])
                .unwrap(),
            b"MM tracked\0"
        );
        std::fs::write(fixture.root.join("aaa-untracked"), "new\n").unwrap();
        assert_eq!(
            fixture.query("status", &["--porcelain", "-z"]).unwrap(),
            b"MM tracked\0?? aaa-untracked\0"
        );
        assert_eq!(
            fixture.query("diff-index", &["--cached", "--quiet", "HEAD", "--"]),
            Err(1)
        );
        std::fs::remove_file(fixture.root.join("tracked")).unwrap();
        assert_eq!(
            fixture
                .query("status", &["--porcelain", "-z", "-uno"])
                .unwrap(),
            b"MD tracked\0"
        );
    }

    #[test]
    fn staged_deletions_and_untracked_files_can_share_a_path() {
        let fixture = Fixture::new(&[("tracked", "original\n")]);
        fixture.set_index(&[]);
        assert_eq!(
            fixture.query("status", &["--porcelain", "-z"]).unwrap(),
            b"D  tracked\0?? tracked\0"
        );
        assert_eq!(
            fixture
                .query("status", &["--porcelain", "-z", "-uno"])
                .unwrap(),
            b"D  tracked\0"
        );
        let v2 = fixture.query("status", &["--porcelain=2", "-z"]).unwrap();
        assert!(v2.starts_with(b"1 D. N... "));
        assert!(v2.ends_with(b" tracked\0? tracked\0"));
        assert_eq!(
            fixture.query("diff-index", &["--cached", "--quiet", "HEAD", "--"]),
            Err(1)
        );
    }

    #[test]
    fn untracked_files_respect_ignores_collapsing_and_existence_status() {
        let fixture = Fixture::new(&[(".gitignore", "ignored\n"), ("tracked", "text\n")]);
        assert_eq!(
            fixture.query(
                "ls-files",
                &[
                    "--others",
                    "--exclude-standard",
                    "--directory",
                    "--no-empty-directory",
                    "--error-unmatch",
                    "--",
                    ":/"
                ]
            ),
            Err(1)
        );
        std::fs::write(fixture.root.join("ignored"), "hidden").unwrap();
        std::fs::create_dir(fixture.root.join("newdir")).unwrap();
        std::fs::write(fixture.root.join("newdir/new"), "new").unwrap();
        assert_eq!(
            fixture
                .query("status", &["--porcelain", "-z", "-unormal"])
                .unwrap(),
            b"?? newdir/\0"
        );
        assert_eq!(
            fixture
                .query("ls-files", &["-o", "--exclude-standard"])
                .unwrap(),
            b"newdir/new\n"
        );
        let ignored = fixture
            .query("status", &["--porcelain=2", "-uno", "--ignored"])
            .unwrap();
        assert_eq!(ignored, b"! ignored\n");
        let mut repo = fixture.repo.clone();
        repo.config_snapshot_mut()
            .append_config(["status.showUntrackedFiles=no"], gix::config::Source::Api)
            .unwrap();
        assert!(
            query(&repo, &fixture.root, "status", &["--porcelain".into()])
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            query(
                &repo,
                &fixture.root,
                "status",
                &["--porcelain".into(), "-unormal".into()]
            )
            .unwrap(),
            b"?? newdir/\n"
        );
    }

    #[test]
    fn renamed_paths_keep_porcelain_source_fields_and_subdirectory_paths() {
        let fixture = Fixture::new(&[("old name", "contents\n")]);
        fixture.set_index(&[("new name", "contents\n")]);
        std::fs::rename(fixture.root.join("old name"), fixture.root.join("new name")).unwrap();
        assert_eq!(
            fixture.query("status", &["--porcelain", "-z"]).unwrap(),
            b"R  new name\0old name\0"
        );
        std::fs::create_dir(fixture.root.join("nested")).unwrap();
        let output = query(
            &fixture.repo,
            &fixture.root.join("nested"),
            "status",
            &["--porcelain=2".to_owned(), "-uno".to_owned()],
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.starts_with("2 R. N... "), "{output}");
        assert!(
            output.ends_with(" R100 ../new name\t../old name\n"),
            "{output}"
        );
        let outside = query(
            &fixture.repo,
            fixture.root.parent().unwrap(),
            "status",
            &["--porcelain".to_owned(), "-z".to_owned()],
        )
        .unwrap();
        assert_eq!(outside, b"R  new name\0old name\0");
        let mut repo = fixture.repo.clone();
        repo.config_snapshot_mut()
            .append_config(["status.renames=false"], gix::config::Source::Api)
            .unwrap();
        let args = ["--porcelain".into(), "-z".into()];
        assert_eq!(
            query(&repo, &fixture.root, "status", &args).unwrap(),
            b"A  new name\0D  old name\0"
        );
        repo.config_snapshot_mut()
            .append_config(
                ["status.renames=true", "merge.renames=false"],
                gix::config::Source::Api,
            )
            .unwrap();
        assert_eq!(
            query(&repo, &fixture.root, "status", &args).unwrap(),
            b"R  new name\0old name\0"
        );
    }

    #[test]
    fn status_uses_gix_normalization_without_external_diff_drivers() {
        let fixture = Fixture::new(&[
            (".gitattributes", "tracked filter=probe diff=probe\n"),
            ("tracked", "ORIGINAL\n"),
        ]);
        std::fs::write(fixture.root.join("tracked"), "original\n").unwrap();
        let mut repo = fixture.repo.clone();
        repo.config_snapshot_mut()
            .append_config(
                [
                    "FILTER.probe.clean=tr a-z A-Z",
                    "filter.probe.required=true",
                    "diff.probe.textconv=echo forbidden > spawned-textconv",
                    "diff.external=echo forbidden > spawned-diff",
                    "core.fsmonitor=echo forbidden > spawned-fsmonitor",
                ],
                gix::config::Source::Api,
            )
            .unwrap();
        let args = ["--porcelain".to_owned(), "-z".to_owned()];
        assert_eq!(query(&repo, &fixture.root, "status", &args).unwrap(), b"");
        std::fs::write(fixture.root.join("tracked"), "changed\n").unwrap();
        assert_eq!(
            query(&repo, &fixture.root, "status", &args).unwrap(),
            b" M tracked\0"
        );
        for path in ["spawned-textconv", "spawned-diff", "spawned-fsmonitor"] {
            assert!(!fixture.root.join(path).exists());
        }
    }

    #[test]
    fn path_quoting_preserves_non_ascii_and_escapes_control_characters() {
        assert_eq!(quote_path("café".as_bytes(), false), "café".as_bytes());
        assert_eq!(quote_path(b"a\tb\nc\\d\"", false), br#""a\tb\nc\\d\"""#);
        assert_eq!(relative_path(b"a/other", b"a/nested"), b"../other");
    }

    #[test]
    fn conflicts_have_unmerged_porcelain_status_and_stage_metadata() {
        let fixture = Fixture::new(&[("conflict", "base\n")]);
        let mut index = gix::index::State::new(fixture.repo.object_hash());
        for (stage, content) in [(1, "base\n"), (2, "ours\n"), (3, "theirs\n")] {
            index.dangerously_push_entry(
                Default::default(),
                fixture.repo.write_blob(content).unwrap().detach(),
                gix::index::entry::Flags::from_bits_retain(stage << 12),
                gix::index::entry::Mode::FILE,
                "conflict".into(),
            );
        }
        gix::index::File::from_state(index, fixture.repo.index_path())
            .write(Default::default())
            .unwrap();
        assert_eq!(
            fixture.query("status", &["--porcelain", "-z"]).unwrap(),
            b"UU conflict\0"
        );
        let output =
            String::from_utf8(fixture.query("status", &["--porcelain=2"]).unwrap()).unwrap();
        assert!(
            output.starts_with("u UU N... 100644 100644 100644 100644 "),
            "{output}"
        );
        assert!(output.ends_with(" conflict\n"), "{output}");
    }

    #[test]
    fn submodule_status_honors_ignore_precedence_and_gix_normalization() {
        use std::io::Write as _;
        let fixture = Fixture::new(&[]);
        let child = Fixture::new(&[
            (".gitattributes", "tracked filter=probe\n"),
            ("tracked", "original\n"),
        ]);
        let child_commit = child.repo.head_id().unwrap().detach();
        let subdir = fixture.root.join("sub");
        std::fs::rename(&child.root, &subdir).unwrap();
        std::fs::write(subdir.join("tracked"), "modified\n").unwrap();
        let tree = fixture
            .repo
            .write_object(&gix::objs::Tree {
                entries: vec![gix::objs::tree::Entry {
                    mode: gix::objs::tree::EntryKind::Commit.into(),
                    filename: "sub".into(),
                    oid: child_commit,
                }],
            })
            .unwrap()
            .detach();
        fixture
            .repo
            .index_from_tree(&tree)
            .unwrap()
            .write(Default::default())
            .unwrap();
        assert_eq!(
            fixture.query("status", &["--porcelain", "-z"]).unwrap(),
            b"AM sub\0"
        );
        assert_eq!(
            fixture
                .query("status", &["--porcelain", "-z", "--ignore-submodules=all"])
                .unwrap(),
            b""
        );
        assert_eq!(
            fixture
                .query(
                    "status",
                    &["--porcelain", "-z", "--ignore-submodules=dirty"]
                )
                .unwrap(),
            b"A  sub\0"
        );
        let mut repo = fixture.repo.clone();
        repo.config_snapshot_mut()
            .append_config(["diff.ignoreSubmodules=all"], gix::config::Source::Api)
            .unwrap();
        let args = ["--porcelain".into(), "-z".into(), "-uno".into()];
        assert_eq!(
            query(&repo, &fixture.root, "status", &args).unwrap(),
            b"A  sub\0"
        );
        std::fs::write(
            fixture.root.join(".gitmodules"),
            "[submodule \"sub\"]\npath = sub\nurl = nowhere\n",
        )
        .unwrap();
        assert_eq!(
            query(&repo, &fixture.root, "status", &args).unwrap(),
            b"A  sub\0"
        );
        repo.config_snapshot_mut()
            .append_config(["submodule.sub.ignore=none"], gix::config::Source::Api)
            .unwrap();
        assert_eq!(
            query(&repo, &fixture.root, "status", &args).unwrap(),
            b"AM sub\0"
        );
        let mut cli_args = args.to_vec();
        cli_args.push("--ignore-submodules=all".into());
        assert_eq!(
            query(&repo, &fixture.root, "status", &cli_args).unwrap(),
            b""
        );
        repo.config_snapshot_mut()
            .append_config(
                ["diff.ignoreSubmodules=none", "submodule.sub.ignore=all"],
                gix::config::Source::Api,
            )
            .unwrap();
        assert_eq!(
            query(&repo, &fixture.root, "status", &args).unwrap(),
            b"A  sub\0"
        );
        *cli_args.last_mut().unwrap() = "--ignore-submodules=none".into();
        assert_eq!(
            query(&repo, &fixture.root, "status", &cli_args).unwrap(),
            b"AM sub\0"
        );
        std::fs::OpenOptions::new()
            .append(true)
            .open(subdir.join(".git/config"))
            .unwrap()
            .write_all(b"\n[filter \"probe\"]\n\tclean = tr A-Z a-z\n\trequired = true\n")
            .unwrap();
        std::fs::write(subdir.join("tracked"), "ORIGINAL\n").unwrap();
        assert_eq!(
            query(&repo, &fixture.root, "status", &cli_args).unwrap(),
            b"A  sub\0"
        );
        assert_eq!(fixture.query("diff", &["--quiet"]), Ok(Vec::new()));
        std::fs::write(subdir.join("tracked"), "MODIFIED\n").unwrap();
        assert_eq!(
            query(&repo, &fixture.root, "status", &cli_args).unwrap(),
            b"AM sub\0"
        );
        assert_eq!(
            fixture.query("diff-index", &["--quiet", "HEAD", "--"]),
            Err(1)
        );
        *cli_args.last_mut().unwrap() = "--ignore-submodules=dirty".into();
        assert_eq!(
            query(&repo, &fixture.root, "status", &cli_args).unwrap(),
            b"A  sub\0"
        );
        *cli_args.last_mut().unwrap() = "--ignore-submodules=all".into();
        assert_eq!(
            query(&repo, &fixture.root, "status", &cli_args).unwrap(),
            b""
        );
    }
}
