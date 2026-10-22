//! Read-only Git queries used by bundled scripts on both execution backends.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use gix::bstr::ByteSlice;

type QueryResult = Result<Vec<u8>, i32>;

pub(crate) fn query(
    cwd: &Path,
    environment: &[(OsString, OsString)],
    arguments: &[String],
) -> QueryResult {
    let variable = |key: &str| super::environment_variable(environment, key);
    let mut routing_args = Vec::new();
    let mut overrides = Vec::new();
    let mut worktree = variable("GIT_WORK_TREE").map(PathBuf::from);
    let mut index = 0;
    while let Some(arg) = arguments.get(index) {
        if !arg.starts_with('-') {
            break;
        }
        let (name, attached) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(k, v)| (k, Some(v)));
        match name {
            "-C" | "--git-dir" | "--work-tree" | "--namespace" | "-c" | "--config-env" => {
                let value = match attached {
                    Some(value) => value,
                    None => {
                        index += 1;
                        arguments.get(index).ok_or(2)?.as_str()
                    }
                };
                match name {
                    "-c" => overrides.push(value.to_owned()),
                    "--config-env" => {
                        let (key, env) = value.split_once('=').ok_or(2)?;
                        overrides.push(format!(
                            "{key}={}",
                            variable(env).and_then(OsStr::to_str).ok_or(2)?
                        ));
                    }
                    "--work-tree" => worktree = Some(value.into()),
                    _ => routing_args.extend([name.to_owned(), value.to_owned()]),
                }
            }
            "--bare" => routing_args.push(arg.clone()),
            "--no-pager"
            | "--paginate"
            | "-p"
            | "-P"
            | "--no-optional-locks"
            | "--no-replace-objects"
            | "--literal-pathspecs"
            | "--glob-pathspecs"
            | "--noglob-pathspecs"
            | "--icase-pathspecs" => {}
            _ if arg.starts_with("-C") && arg.len() > 2 => {
                routing_args.extend(["-C".into(), arg[2..].into()])
            }
            _ if arg.starts_with("-c") && arg.len() > 2 => overrides.push(arg[2..].into()),
            _ => return Err(2),
        }
        index += 1;
    }
    let command = arguments.get(index).ok_or(2)?.as_str();
    let args = &arguments[index + 1..];
    if command == "help" && matches!(args, [arg] if arg == "--config" || arg == "-g")
        || matches!(command, "difftool" | "mergetool") && args == ["--tool-help"]
    {
        // Git's installed tools and full configuration catalog are not exposed by gix.
        return Err(super::USE_CLI);
    }
    let env_count = variable("GIT_CONFIG_COUNT")
        .map(|s| s.to_str().ok_or(2)?.parse::<usize>().map_err(|_| 2))
        .transpose()?
        .unwrap_or(0);
    if env_count > 1024 {
        return Err(2);
    }
    let mut env_overrides = Vec::new();
    for idx in 0..env_count {
        let key = variable(&format!("GIT_CONFIG_KEY_{idx}"))
            .and_then(OsStr::to_str)
            .ok_or(2)?;
        let value = variable(&format!("GIT_CONFIG_VALUE_{idx}"))
            .and_then(OsStr::to_str)
            .ok_or(2)?;
        env_overrides.push(format!("{key}={value}"));
    }
    env_overrides.append(&mut overrides);
    // These snapshot overrides need Git until gix accepts them explicitly.
    if [
        "GIT_COMMON_DIR",
        "GIT_CONFIG_PARAMETERS",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ]
    .iter()
    .any(|key| variable(key).is_some_and(|value| !value.is_empty()))
    {
        return Err(super::USE_CLI);
    }
    let routing_env = environment
        .iter()
        .filter(|(name, _)| !super::environment_name_matches(name, "GIT_CONFIG_COUNT"))
        .cloned()
        .collect::<Vec<_>>();
    let routing = super::Routing::new(cwd, &routing_args, &routing_env).ok_or(1)?;
    for (env, key) in [
        ("GIT_INDEX_FILE", "gitoxide.core.indexFile"),
        ("GIT_SHALLOW_FILE", "gitoxide.core.shallowFile"),
    ] {
        if let Some(value) = variable(env) {
            let path = routing.cwd.join(value);
            env_overrides.push(format!("{key}={}", path.to_str().ok_or(2)?));
        }
    }
    let mut repo = routing.open_with_config(&env_overrides);
    if let Some(repo) = repo.as_mut() {
        if !super::reference_storage_supported(repo) {
            return Err(1);
        }
    }
    let mut config = configuration(&routing.cwd, environment, repo.as_ref())?;
    for value in &env_overrides {
        let (key, value) = value
            .split_once('=')
            .map_or((value.as_str(), None), |(k, v)| {
                (k, Some(v.as_bytes().as_bstr()))
            });
        let key = gix::config::KeyRef::parse_unvalidated(key.as_bytes().as_bstr()).ok_or(2)?;
        config
            .section_mut_or_create_new(key.section_name, key.subsection_name)
            .map_err(|_| 2)?
            .push(key.value_name, value)
            .map_err(|_| 2)?;
    }
    if let Some(repo) = repo.as_mut() {
        let mut snapshot = repo.config_snapshot_mut();
        *snapshot = config;
        snapshot
            .append_config(
                ["gitoxide.objects.noReplace=true"],
                gix::config::Source::Api,
            )
            .map_err(|_| 2)?;
        snapshot.commit().map_err(|_| 1)?;
        if let Some(worktree) = worktree {
            repo.set_workdir(Some(
                gix::path::realpath(routing.cwd.join(worktree)).map_err(|_| 1)?,
            ))
            .map_err(|_| 1)?;
        } else if repo.is_bare() {
            repo.set_workdir(None).map_err(|_| 1)?;
        } else if let Some(worktree) = repo.config_snapshot().string("core.worktree") {
            repo.set_workdir(Some(
                gix::path::realpath(
                    repo.git_dir()
                        .join(gix::path::from_bstr(worktree).map_err(|_| 1)?),
                )
                .map_err(|_| 1)?,
            ))
            .map_err(|_| 1)?;
        }
    } else if command == "config" {
        return config_query(&config, args);
    }
    let repo = repo.ok_or(1)?;
    match command {
        "config" => config_query(&repo.config_snapshot(), args),
        "rev-parse" => rev_parse(&repo, &routing.cwd, args),
        "symbolic-ref" => {
            let name = args.iter().find(|arg| !arg.starts_with('-')).ok_or(2)?;
            let reference = repo.find_reference(name.as_str()).map_err(|_| 1)?;
            let reference_target = reference.target();
            let target = reference_target.try_name().ok_or(1)?;
            let name = if args.iter().any(|a| a == "--short") {
                target.shorten()
            } else {
                target.as_bstr()
            };
            Ok([name.as_ref(), b"\n".as_slice()].concat())
        }
        "for-each-ref" | "tag" => refs(&repo, command, args),
        "remote" if args.is_empty() => {
            let mut out = Vec::new();
            for name in repo.remote_names() {
                line(&mut out, name.as_ref());
            }
            Ok(out)
        }
        "worktree" if args == ["list", "--porcelain"] => {
            let mut out = Vec::new();
            let main = repo.main_repo().map_err(|_| 1)?;
            for path in std::iter::once(main.workdir().unwrap_or(main.git_dir()).to_owned()).chain(
                repo.worktrees()
                    .map_err(|_| 1)?
                    .into_iter()
                    .filter_map(|w| w.base().ok()),
            ) {
                out.extend_from_slice(b"worktree ");
                line(&mut out, path.as_os_str().as_encoded_bytes());
            }
            Ok(out)
        }
        "submodule" if args.is_empty() => {
            let mut out = Vec::new();
            for sub in repo.submodules().map_err(|_| 1)?.into_iter().flatten() {
                let path = sub.path().map_err(|_| 1)?;
                let id = sub
                    .index_id()
                    .map_err(|_| 1)?
                    .unwrap_or_else(|| gix::ObjectId::null(repo.object_hash()));
                out.extend_from_slice(format!(" {id} ").as_bytes());
                line(&mut out, path.as_ref());
            }
            Ok(out)
        }
        "status" | "ls-files" | "diff" | "diff-index" => {
            super::status::query(&repo, &routing.cwd, command, args)
        }
        "log" | "rev-list" | "show" | "describe" | "stash" | "reflog" => {
            super::history::query(&repo, command, args)
        }
        _ => Err(2),
    }
}

fn line(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(value);
    output.push(b'\n');
}

fn configuration(
    cwd: &Path,
    environment: &[(OsString, OsString)],
    repo: Option<&gix::Repository>,
) -> Result<gix::config::File, i32> {
    use gix::config::{
        Source,
        file::{Metadata, includes, init},
    };
    let variable = |key: &str| super::environment_variable(environment, key);
    let home = variable("HOME")
        .or_else(|| cfg!(windows).then(|| variable("USERPROFILE")).flatten())
        .map(|path| {
            if path.is_empty() {
                PathBuf::from("/")
            } else {
                cwd.join(path)
            }
        });
    let mut paths = Vec::new();
    let no_system = variable("GIT_CONFIG_NOSYSTEM")
        .and_then(OsStr::to_str)
        .map(|value| {
            !matches!(
                value.to_ascii_lowercase().as_str(),
                "" | "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(false);
    if !no_system {
        let path = variable("GIT_CONFIG_SYSTEM").or_else(|| {
            if cfg!(windows) {
                gix::path::env::system_config().map(Path::as_os_str)
            } else {
                Some(OsStr::new("/etc/gitconfig"))
            }
        });
        if let Some(path) = path.filter(|path| !path.is_empty()) {
            paths.push(Metadata::from(Source::System).at(cwd.join(path)));
        }
    }
    if let Some(path) = variable("GIT_CONFIG_GLOBAL") {
        if !path.is_empty() {
            paths.push(Metadata::from(Source::User).at(cwd.join(path)));
        }
    } else {
        if let Some(xdg) = variable("XDG_CONFIG_HOME")
            .filter(|path| !path.is_empty())
            .map(|path| cwd.join(path))
            .or_else(|| home.as_ref().map(|h| h.join(".config")))
        {
            paths.push(Metadata::from(Source::User).at(xdg.join("git/config")));
        }
        if let Some(home) = &home {
            paths.push(Metadata::from(Source::User).at(home.join(".gitconfig")));
        }
    }
    if let Some(repo) = repo {
        paths.push(Metadata::from(Source::Local).at(repo.common_dir().join("config")));
        if repo.config_snapshot().boolean("extensions.worktreeConfig") == Some(true) {
            paths.push(Metadata::from(Source::Worktree).at(repo.git_dir().join("config.worktree")));
        }
    }
    let branch = repo.and_then(|r| r.head_name().ok().flatten());
    let mut follow = includes::Options::follow_without_conditional(home.as_deref());
    follow.conditional = includes::conditional::Context {
        git_dir: repo.map(gix::Repository::git_dir),
        branch_name: branch.as_ref().map(|name| name.as_ref()),
    };
    let options = init::Options {
        includes: follow,
        ..Default::default()
    };
    gix::config::File::from_paths_metadata_buf(
        &mut paths.into_iter(),
        &mut Vec::new(),
        false,
        options,
    )
    .map(|config| config.unwrap_or_default())
    .map_err(|_| 1)
}

fn config_query(config: &gix::config::File, args: &[String]) -> QueryResult {
    let mut nul = false;
    let mut list = false;
    let mut regexp = false;
    let mut boolean = false;
    let mut names_only = false;
    let mut positional = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-z" | "--null" => nul = true,
            "-l" | "--list" => list = true,
            "-lz" | "-zl" => {
                nul = true;
                list = true;
            }
            "--get-regexp" => regexp = true,
            "--get" => {}
            "--type" if args.next().map(String::as_str) == Some("bool") => boolean = true,
            "--bool" => boolean = true,
            "--name-only" => names_only = true,
            _ if arg.starts_with('-') => return Err(2),
            _ => positional.push(arg),
        }
    }
    if !list && !regexp {
        if positional.len() != 1 {
            return Err(2);
        }
        let key = positional[0].as_str();
        let value = if boolean {
            config
                .boolean(key)
                .map_err(|_| 1)?
                .ok_or(1)?
                .to_string()
                .into_bytes()
        } else {
            config.string(key).ok_or(1)?.to_vec()
        };
        return Ok([value, vec![if nul { 0 } else { b'\n' }]].concat());
    }
    let regex = if regexp {
        let pattern = positional.first().ok_or(2)?;
        Some(
            pcre2::utf32::RegexBuilder::new()
                .caseless(true)
                .build(pattern.chars().collect::<Vec<_>>().into_boxed_slice())
                .map_err(|_| 2)?,
        )
    } else {
        None
    };
    let mut out = Vec::new();
    for section in config.sections() {
        let header = section.header();
        let mut prefix = header.name().to_str().map_err(|_| 1)?.to_ascii_lowercase();
        if let Some(sub) = header.subsection_name() {
            prefix.push('.');
            prefix.push_str(sub.to_str().map_err(|_| 1)?);
        }
        let mut seen = BTreeSet::new();
        for name in section.value_names() {
            if !seen.insert(name.to_ascii_lowercase()) {
                continue;
            }
            let key = format!("{prefix}.{}", name.to_ascii_lowercase());
            if regex.as_ref().is_some_and(|re| {
                !re.is_match(&key.chars().collect::<Vec<_>>())
                    .unwrap_or(false)
            }) {
                continue;
            }
            for value in section.values(&name) {
                out.extend_from_slice(key.as_bytes());
                if !names_only {
                    out.push(if nul {
                        b'\n'
                    } else if list {
                        b'='
                    } else {
                        b' '
                    });
                    out.extend_from_slice(value.as_ref());
                }
                out.push(if nul { 0 } else { b'\n' });
            }
        }
    }
    if regexp && out.is_empty() {
        Err(1)
    } else {
        Ok(out)
    }
}

fn rev_parse(repo: &gix::Repository, cwd: &Path, args: &[String]) -> QueryResult {
    let mut out = Vec::new();
    let mut short = false;
    let mut abbrev = false;
    for arg in args {
        let value = match arg.as_str() {
            "--quiet" | "--verify" => continue,
            "--short" => {
                short = true;
                continue;
            }
            "--abbrev-ref" => {
                abbrev = true;
                continue;
            }
            "--git-dir" | "--absolute-git-dir" => {
                repo.git_dir().as_os_str().as_encoded_bytes().to_vec()
            }
            "--git-common-dir" => repo.common_dir().as_os_str().as_encoded_bytes().to_vec(),
            "--show-toplevel" => repo
                .workdir()
                .ok_or(1)?
                .as_os_str()
                .as_encoded_bytes()
                .to_vec(),
            "--is-bare-repository" => repo.is_bare().to_string().into_bytes(),
            "--is-inside-git-dir" => path_is_within(cwd, repo.git_dir()).to_string().into_bytes(),
            "--is-inside-work-tree" => {
                (repo.workdir().is_some_and(|root| path_is_within(cwd, root))
                    && !path_is_within(cwd, repo.git_dir()))
                .to_string()
                .into_bytes()
            }
            _ if arg.starts_with('-') => return Err(2),
            _ if abbrev => {
                let name = if arg == "@{upstream}" || arg == "@{u}" {
                    repo.head_ref()
                        .map_err(|_| 1)?
                        .ok_or(1)?
                        .remote_tracking_ref_name(gix::remote::Direction::Fetch)
                        .ok_or(1)?
                        .map_err(|_| 1)?
                } else {
                    repo.find_reference(arg.as_str())
                        .map_err(|_| 1)?
                        .name()
                        .to_owned()
                };
                name.shorten().to_vec()
            }
            _ => match repo.rev_parse_single(arg.as_str()) {
                Ok(id) => if short {
                    id.shorten().map_err(|_| 1)?.to_string()
                } else {
                    id.to_string()
                }
                .into_bytes(),
                Err(_) if !out.is_empty() => {
                    // Git includes the unresolved HEAD token after repository metadata.
                    line(&mut out, arg.as_bytes());
                    break;
                }
                Err(_) => return Err(1),
            },
        };
        line(&mut out, &value);
    }
    Ok(out)
}

fn path_is_within(path: &Path, directory: &Path) -> bool {
    #[cfg(windows)]
    {
        // Windows accepts both C:\\... and \\\\?\\C:\\... for the same directory.
        // gix may return either spelling when opening a discovered repository.
        path.canonicalize().is_ok_and(|path| {
            directory
                .canonicalize()
                .is_ok_and(|directory| path.starts_with(directory))
        })
    }
    #[cfg(not(windows))]
    path.starts_with(directory)
}

fn refs(repo: &gix::Repository, command: &str, args: &[String]) -> QueryResult {
    let mut format = "%(refname)";
    let mut patterns = Vec::new();
    let mut sort_time = None;
    let mut sort_short = false;
    for arg in args {
        if let Some(value) = arg.strip_prefix("--format=") {
            format = value;
        } else if let Some(sort) = arg.strip_prefix("--sort=") {
            sort_time = matches!(sort, "-committerdate" | "-creatordate").then_some(sort);
            sort_short = sort == "refname:strip=3";
        } else if arg.starts_with('-') {
            return Err(2);
        } else {
            patterns.push(arg.as_str());
        }
    }
    if command == "tag" {
        patterns.push("refs/tags/");
        format = "%(refname:strip=2)";
    }
    let platform = repo.references().map_err(|_| 1)?;
    let mut refs = Vec::new();
    for reference in platform.all().map_err(|_| 1)? {
        let mut reference = reference.map_err(|_| 1)?;
        let name = reference.name().as_bstr().to_vec();
        if !patterns.is_empty()
            && !patterns.iter().any(|pattern| {
                if let Some((start, end)) = pattern.split_once('*') {
                    name.starts_with(start.as_bytes()) && name.ends_with(end.as_bytes())
                } else {
                    name.starts_with(pattern.as_bytes())
                }
            })
        {
            continue;
        }
        let time = if let Some(sort) = sort_time {
            reference
                .follow_to_object()
                .ok()
                .and_then(|id| id.object().ok())
                .and_then(|object| match object.kind {
                    gix::objs::Kind::Commit => {
                        object.into_commit().time().ok().map(|time| time.seconds)
                    }
                    gix::objs::Kind::Tag if sort == "-creatordate" => object
                        .into_tag()
                        .tagger()
                        .ok()
                        .flatten()
                        .map(|tagger| tagger.seconds()),
                    _ => None,
                })
                .unwrap_or(0)
        } else {
            0
        };
        refs.push((time, name));
    }
    refs.sort_by(|a, b| {
        b.0.cmp(&a.0).then_with(|| {
            if sort_short {
                a.1.split(|b| *b == b'/')
                    .skip(3)
                    .cmp(b.1.split(|b| *b == b'/').skip(3))
            } else {
                a.1.cmp(&b.1)
            }
        })
    });
    let mut out = Vec::new();
    for (_, name) in refs {
        let formatted = format
            .as_bytes()
            .replace(
                b"%(refname:strip=2)",
                name.splitn(3, |b| *b == b'/').nth(2).unwrap_or_default(),
            )
            .replace(
                b"%(refname:strip=3)",
                name.splitn(4, |b| *b == b'/').nth(3).unwrap_or_default(),
            )
            .replace(b"%(refname)", &name)
            .replace(b"%09", b"\t");
        line(&mut out, &formatted);
    }
    Ok(out)
}

#[test]
fn refs_sort_tags_by_tagger_and_lightweight_tags_by_committer() {
    let temp = super::test_support::TemporaryDirectory::new();
    let mut repo = gix::ThreadSafeRepository::init_opts(
        temp.path().join("repo"),
        gix::create::Kind::WithWorktree,
        Default::default(),
        gix::open::Options::isolated(),
    )
    .unwrap()
    .to_thread_local();
    repo.refs.write_reflog = gix::refs::store::WriteReflog::Disable;
    let signature = |seconds| gix::actor::Signature {
        name: "Fish Test".into(),
        email: "fish@example.invalid".into(),
        time: gix::date::Time { seconds, offset: 0 },
    };
    let commit = |seconds| {
        repo.write_object(&gix::objs::Commit {
            tree: repo.empty_tree().id,
            parents: Default::default(),
            author: signature(seconds),
            committer: signature(seconds),
            encoding: None,
            message: "fixture\n".into(),
            extra_headers: Vec::new(),
        })
        .unwrap()
        .detach()
    };
    let reference = |name: &str, id| {
        repo.reference(
            name,
            id,
            gix::refs::transaction::PreviousValue::Any,
            "fixture",
        )
        .unwrap();
    };
    let old = commit(100);
    let new = commit(200);
    reference("refs/heads/old", old);
    reference("refs/heads/new", new);
    reference("refs/tags/lightweight", new);
    for (name, target, seconds) in [("annotated-new", old, 300), ("annotated-old", new, 100)] {
        let tag = repo
            .write_object(&gix::objs::Tag {
                target,
                target_kind: gix::objs::Kind::Commit,
                name: name.into(),
                tagger: Some(signature(seconds)),
                message: "tag\n".into(),
                signature: None,
            })
            .unwrap()
            .detach();
        reference(&format!("refs/tags/{name}"), tag);
    }
    assert_eq!(
        refs(&repo, "tag", &["--sort=-creatordate".into()]),
        Ok(b"annotated-new\nlightweight\nannotated-old\n".to_vec())
    );
    assert_eq!(
        refs(
            &repo,
            "for-each-ref",
            &[
                "--sort=-committerdate".into(),
                "--format=%(refname:strip=2)".into(),
                "refs/heads/".into()
            ]
        ),
        Ok(b"new\nold\n".to_vec())
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).into()).collect()
    }

    #[test]
    fn configuration_snapshot_names_follow_platform_case_rules() {
        let temporary = super::super::test_support::TemporaryDirectory::new();
        let root = temporary.path();
        std::fs::write(root.join("global"), "[alias]\n selected = file\n").unwrap();
        let environment = vec![
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), root.join("global").into()),
            ("GIT_CONFIG_COUNT".into(), "0".into()),
            ("git_config_count".into(), "1".into()),
            ("git_config_key_0".into(), "alias.selected".into()),
            ("git_config_value_0".into(), "snapshot".into()),
        ];
        assert_eq!(
            query(root, &environment, &args(&["config", "alias.selected"])),
            Ok(if cfg!(windows) {
                "snapshot\n"
            } else {
                "file\n"
            }
            .as_bytes()
            .to_vec())
        );
    }

    #[test]
    fn user_configuration_uses_snapshot_home_and_windows_profile() {
        let temporary = super::super::test_support::TemporaryDirectory::new();
        let root = temporary.path();
        for (directory, value) in [("profile 文", "profile"), ("home", "home")] {
            std::fs::create_dir(root.join(directory)).unwrap();
            std::fs::write(
                root.join(directory).join(".gitconfig"),
                format!("[include]\n path = ~/included\n[alias]\n location = {value}\n"),
            )
            .unwrap();
            std::fs::write(
                root.join(directory).join("included"),
                format!("[alias]\n included = {value}\n"),
            )
            .unwrap();
        }
        let mut environment = vec![
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("USERPROFILE".into(), "profile 文".into()),
        ];
        for key in ["alias.location", "alias.included"] {
            assert_eq!(
                query(root, &environment, &args(&["config", key])),
                if cfg!(windows) {
                    Ok(b"profile\n".to_vec())
                } else {
                    Err(1)
                }
            );
        }
        environment.push(("HOME".into(), "home".into()));
        assert_eq!(
            query(root, &environment, &args(&["config", "alias.included"])),
            Ok(b"home\n".to_vec())
        );
    }

    #[test]
    fn missing_library_apis_request_cli_without_masking_repository_errors() {
        let temporary = super::super::test_support::TemporaryDirectory::new();
        let repository = temporary.repository("repo");
        let root = repository.workdir().unwrap();
        for arguments in [
            args(&["help", "--config"]),
            args(&["help", "-g"]),
            args(&["difftool", "--tool-help"]),
            args(&["mergetool", "--tool-help"]),
        ] {
            assert_eq!(query(root, &[], &arguments), Err(125), "{arguments:?}");
        }
        for name in [
            "GIT_COMMON_DIR",
            "GIT_CONFIG_PARAMETERS",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        ] {
            assert_eq!(
                query(
                    root,
                    &[(name.into(), "unsupported".into())],
                    &args(&["rev-parse", "--git-dir"]),
                ),
                Err(125),
                "{name}"
            );
        }
    }

    #[test]
    fn configuration_uses_exported_paths_includes_and_overrides() {
        let temp = super::super::test_support::TemporaryDirectory::new();
        let root = temp.path().canonicalize().unwrap();
        let repo = gix::ThreadSafeRepository::init_opts(
            root.join("repo"),
            gix::create::Kind::WithWorktree,
            Default::default(),
            gix::open::Options::isolated(),
        )
        .unwrap()
        .to_thread_local();
        std::fs::write(root.join("included"), "[alias]\n frominclude = show\n").unwrap();
        std::fs::write(
            root.join("global"),
            "[alias]\n same = global\n[include]\n path = included\n",
        )
        .unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(repo.git_dir().join("config"))
            .unwrap();
        use std::io::Write;
        writeln!(
            file,
            "[alias]\n same = local\n[remote \"origin\"]\n url = a\n fetch = one\n fetch = two"
        )
        .unwrap();
        let env = vec![
            (
                "GIT_CONFIG_GLOBAL".into(),
                root.join("global").into_os_string(),
            ),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ];
        assert_eq!(
            query(&root, &env, &args(&["-C", "repo", "config", "alias.same"])),
            Ok(b"local\n".to_vec())
        );
        assert_eq!(
            query(&root, &env, &args(&["config", "alias.frominclude"])),
            Ok(b"show\n".to_vec())
        );
        assert_eq!(
            query(
                &root,
                &env,
                &args(&["-c", "alias.same=cli", "config", "alias.same"])
            ),
            Ok(b"cli\n".to_vec())
        );
        assert_eq!(
            query(
                &root,
                &env,
                &args(&[
                    "-C",
                    "repo",
                    "config",
                    "-z",
                    "--get-regexp",
                    "^remote\\..*\\.fetch$"
                ])
            ),
            Ok(b"remote.origin.fetch\none\0remote.origin.fetch\ntwo\0".to_vec())
        );
        assert_eq!(
            query(
                &root,
                &env,
                &args(&["-C", "repo", "config", "alias.same", "mutation"])
            ),
            Err(2)
        );
        assert_eq!(
            query(&root, &env, &args(&["-C", "repo", "reset", "--hard"])),
            Err(2)
        );
        assert_eq!(
            query(&root, &env, &args(&["-C", "repo", "symbolic-ref", "HEAD"])),
            Ok(b"refs/heads/main\n".to_vec())
        );
    }

    #[test]
    fn relative_configuration_paths_follow_routed_cwd() {
        let temp = super::super::test_support::TemporaryDirectory::new();
        let root = temp.path().canonicalize().unwrap();
        let routed = root.join("routed");
        for (base, value) in [(&root, "parent"), (&routed, "routed")] {
            for (path, key) in [
                ("system", "system"),
                ("global", "global"),
                ("home/.gitconfig", "home"),
                ("home/.config/git/config", "fallback"),
                ("home/included", "included"),
                ("xdg/git/config", "xdg"),
            ] {
                let path = base.join(path);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, format!("[alias]\n {key} = {value}\n")).unwrap();
            }
            std::fs::write(
                base.join("global"),
                format!("[alias]\n global = {value}\n[include]\n path = home/included\n"),
            )
            .unwrap();
            std::fs::write(
                base.join("home/.gitconfig"),
                format!("[alias]\n home = {value}\n[include]\n path = ~/included\n"),
            )
            .unwrap();
        }
        for (exported, key) in [
            (vec![("GIT_CONFIG_SYSTEM", "system")], "alias.system"),
            (vec![("GIT_CONFIG_GLOBAL", "global")], "alias.global"),
            (vec![("GIT_CONFIG_GLOBAL", "global")], "alias.included"),
            (vec![("HOME", "home")], "alias.home"),
            (vec![("HOME", "home")], "alias.fallback"),
            (vec![("HOME", "home")], "alias.included"),
            (vec![("XDG_CONFIG_HOME", "xdg")], "alias.xdg"),
            (
                vec![("HOME", "home"), ("XDG_CONFIG_HOME", "")],
                "alias.fallback",
            ),
        ] {
            let mut environment = vec![("GIT_CONFIG_SYSTEM".into(), "".into())];
            environment.extend(
                exported
                    .iter()
                    .map(|(key, value)| ((*key).into(), (*value).into())),
            );
            for (cwd, arguments) in [
                (&routed, args(&["config", key])),
                (&root, args(&["-C", "routed", "config", key])),
            ] {
                assert_eq!(
                    query(cwd, &environment, &arguments),
                    Ok(b"routed\n".to_vec()),
                    "{exported:?}, {arguments:?}"
                );
            }
        }
        let environment = vec![
            ("GIT_CONFIG_SYSTEM".into(), "".into()),
            ("GIT_CONFIG_GLOBAL".into(), "".into()),
        ];
        assert_eq!(
            query(
                &root,
                &environment,
                &args(&["-C", "routed", "config", "-l"])
            ),
            Ok(Vec::new())
        );
    }

    #[test]
    fn unborn_repository_information_and_unsupported_storage() {
        let temp = super::super::test_support::TemporaryDirectory::new();
        let root = temp.path().canonicalize().unwrap();
        let repo = gix::ThreadSafeRepository::init_opts(
            root.join("repo"),
            gix::create::Kind::WithWorktree,
            Default::default(),
            gix::open::Options::isolated(),
        )
        .unwrap()
        .to_thread_local();
        let env = vec![
            ("GIT_CONFIG_GLOBAL".into(), "".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ];
        assert_eq!(
            query(
                &root,
                &env,
                &args(&[
                    "-C",
                    "repo",
                    "rev-parse",
                    "--is-bare-repository",
                    "--is-inside-work-tree",
                    "HEAD"
                ])
            ),
            Ok(b"false\ntrue\nHEAD\n".to_vec())
        );
        assert_eq!(
            query(
                &root,
                &env,
                &args(&["-C", "repo", "rev-parse", "--verify", "HEAD"])
            ),
            Err(1)
        );
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(repo.git_dir().join("config"))
            .unwrap();
        use std::io::Write;
        writeln!(file, "[extensions]\n refStorage = reftable").unwrap();
        assert_eq!(
            query(
                &root,
                &env,
                &args(&["-C", "repo", "rev-parse", "--git-dir"])
            ),
            Err(1)
        );
    }
}
