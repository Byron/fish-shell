use fish_build_helper::{
    env_var, fish_build_dir, target_os, target_os_is_apple, target_os_is_bsd, target_os_is_cygwin,
    workspace_root,
};
use rsconf::Target;
#[cfg(not(windows))]
use std::path::{Path, PathBuf};

fn main() {
    let is_nightly =
        rustc_version::version_meta().unwrap().channel == rustc_version::Channel::Nightly;
    rsconf::declare_cfg("nightly", is_nightly);

    // Select the backend from Cargo's target, never from the shell used to
    // invoke Cargo (MSYSTEM, PATH, or the host OS). Cygwin remains POSIX.
    rsconf::set_env_value("BUILD_TARGET_TRIPLE", &env_var("TARGET").unwrap());
    rsconf::set_env_value("BUILD_HOST_TRIPLE", &env_var("HOST").unwrap());
    rsconf::set_env_value("BUILD_PROFILE", &env_var("PROFILE").unwrap());
    rsconf::set_env_value("FISH_BUILD_VERSION", &get_version());
    if target_os() == "windows" {
        return;
    }

    setup_paths();

    // Add our default to enable tools that don't go through CMake, like "cargo test" and the
    // language server.

    rsconf::set_env_value(
        "FISH_RESOLVED_BUILD_DIR",
        // If set by CMake, this might include symlinks. Since we want to compare this to the
        // dir fish is executed in we need to canonicalize it.
        fish_build_dir().canonicalize().unwrap().to_str().unwrap(),
    );

    // We need to canonicalize (i.e. realpath) the manifest dir because we want to be able to
    // compare it directly as a string at runtime.
    rsconf::set_env_value(
        "CARGO_MANIFEST_DIR",
        workspace_root().canonicalize().unwrap().to_str().unwrap(),
    );

    fish_build_helper::rebuild_if_embedded_path_changed("share");

    let build = cc::Build::new();
    let mut target = Target::new_from(build).unwrap();
    // Keep verbose mode on until we've ironed out rust build script stuff
    target.set_verbose(true);
    detect_cfgs(&mut target);

    // Test the compilation target, not the build-script host. In particular,
    // Windows GNU targets use the native backend above, not glibc.
    if env_var("CARGO_CFG_TARGET_ENV").as_deref() == Some("gnu")
        && env_var("CARGO_CFG_TARGET_FEATURE")
            .is_some_and(|features| features.split(',').any(|feature| feature == "crt-static"))
    {
        panic!(
            "Statically linking against glibc has unavoidable crashes and is unsupported. Use dynamic linking or link statically against musl."
        );
    }
}

/// Check target system support for certain functionality dynamically when the build is invoked,
/// without their having to be explicitly enabled in the `cargo build --features xxx` invocation.
///
/// We are using [`rsconf::enable_cfg()`] instead of [`rsconf::enable_feature()`] as rust features
/// should be used for things that a user can/would reasonably enable or disable to tweak or coerce
/// behavior, but here we are testing for whether or not things are supported altogether.
///
/// This can be used to enable features that we check for and conditionally compile according to in
/// our own codebase, but [can't be used to pull in dependencies](0) even if they're gated (in
/// `Cargo.toml`) behind a feature we just enabled.
///
/// [0]: https://github.com/rust-lang/cargo/issues/5499
fn detect_cfgs(target: &mut Target) {
    for (name, handler) in [
        // Ignore the first entry, it just sets up the type inference.
        ("", &(|_: &Target| false) as &dyn Fn(&Target) -> bool),
        ("apple", &(|_| target_os_is_apple())),
        ("bsd", &(|_| target_os_is_bsd())),
        ("cygwin", &(|_| target_os_is_cygwin())),
        ("have_eventfd", &|target| target.has_header("sys/eventfd.h")),
        ("have_localeconv_l", &|target| {
            target.has_symbol("localeconv_l")
        }),
        ("have_getexecpath", &|target| {
            target_os().as_str() == "openbsd" && target.has_symbol("getexecpath")
        }),
        ("have_posix_spawn", &|target| {
            if matches!(target_os().as_str(), "openbsd" | "android") {
                // OpenBSD's posix_spawn returns status 127 instead of erroring with ENOEXEC when faced with a
                // shebang-less script. Disable posix_spawn on OpenBSD.
                //
                // Android is broken for unclear reasons
                false
            } else {
                target.has_header("spawn.h")
            }
        }),
        ("small_main_stack", &has_small_stack),
        ("solarish", &|_target| {
            matches!(target_os().as_str(), "illumos" | "solaris")
        }),
        ("using_cmake", &|_| {
            option_env!("FISH_CMAKE_BINARY_DIR").is_some()
        }),
    ] {
        rsconf::declare_cfg(name, handler(target));
    }
}

/// Rust sets the stack size of newly created threads to a sane value, but is at at the mercy of the
/// OS when it comes to the size of the main stack. Some platforms we support default to a tiny
/// 0.5 MiB main stack, which is insufficient for fish's MAX_EVAL_DEPTH/MAX_STACK_DEPTH values.
///
/// 0.5 MiB is small enough that we'd have to drastically reduce MAX_STACK_DEPTH to less than 10, so
/// we instead use a workaround to increase the main thread size.
fn has_small_stack(_: &Target) -> bool {
    #[cfg(not(any(target_os = "ios", target_os = "macos", target_os = "netbsd")))]
    return false;

    // NetBSD 10 also needs this but can't find pthread_get_stacksize_np.
    #[cfg(target_os = "netbsd")]
    return true;

    #[cfg(any(target_os = "ios", target_os = "macos"))]
    {
        use core::ffi;

        unsafe extern "C" {
            unsafe fn pthread_get_stacksize_np(thread: *const ffi::c_void) -> usize;
            unsafe fn pthread_self() -> *const ffi::c_void;
        }

        // build.rs is executed on the main thread, so we are getting the main thread's stack size.
        // Modern macOS versions default to an 8 MiB main stack but legacy OS X have a 0.5 MiB one.
        let stack_size = unsafe { pthread_get_stacksize_np(pthread_self()) };
        const TWO_MIB: usize = 2 * 1024 * 1024 - 1;
        stack_size <= TWO_MIB
    }
}

fn setup_paths() {
    #[cfg(windows)]
    use unix_path::{Path, PathBuf};

    fn overridable_path(
        env_var_name: &str,
        f: impl FnOnce(Option<String>) -> Option<PathBuf>,
    ) -> Option<PathBuf> {
        rsconf::rebuild_if_env_changed(env_var_name);
        let maybe_path = f(env_var(env_var_name));
        if let Some(path) = maybe_path.as_ref() {
            rsconf::set_env_value(env_var_name, path.to_str().unwrap());
        }
        maybe_path
    }

    fn join_if_relative(parent_if_relative: &Path, path: String) -> PathBuf {
        let path = PathBuf::from(path);
        if path.is_relative() {
            parent_if_relative.join(path)
        } else {
            path
        }
    }

    let prefix = overridable_path("PREFIX", |env_prefix| {
        Some(PathBuf::from(env_prefix.unwrap_or("/usr/local".to_owned())))
    })
    .unwrap();

    overridable_path("SYSCONFDIR", |env_sysconfdir| {
        Some(join_if_relative(
            &prefix,
            env_sysconfdir.unwrap_or("/etc/".to_owned()),
        ))
    });

    let datadir = overridable_path("DATADIR", |env_datadir| {
        env_datadir.map(|p| join_if_relative(&prefix, p))
    });
    overridable_path("BINDIR", |env_bindir| {
        env_bindir.map(|p| join_if_relative(&prefix, p))
    });
    overridable_path("DOCDIR", |env_docdir| {
        env_docdir.map(|p| {
            join_if_relative(
                &datadir
                    .expect("Setting DOCDIR without setting DATADIR is not currently supported"),
                p,
            )
        })
    });
}

fn get_version() -> String {
    let version = env!("CARGO_PKG_VERSION");
    #[cfg(feature = "git")]
    let description = git_version();
    #[cfg(not(feature = "git"))]
    let description: Option<String> = None;

    match description
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(description) if description.starts_with(version) => description.to_owned(),
        Some(description) => format!("{version}-g{description}"),
        None => version.to_owned(),
    }
}

#[cfg(feature = "git")]
fn git_version() -> Option<String> {
    // Open only this checkout: an unpacked source archive inside another
    // repository must retain the package version. No Git executable is needed.
    // Disable command-capable configuration, including clean filters used by
    // the dirty check, so obtaining build metadata cannot launch subprocesses.
    let mut repo = gix::open_opts(
        workspace_root(),
        gix::open::Options::isolated().filter_config_section(|_| false),
    )
    .ok()?;
    // gix currently reads loose and packed refs, and does not reject reftable
    // repositories itself. Never report stale loose refs after a migration.
    if repo
        .config_snapshot()
        .string("extensions.refStorage")
        .is_some_and(|storage| !storage.eq_ignore_ascii_case(b"files"))
    {
        return None;
    }
    repo.object_cache_size_if_unset(4 * 1024 * 1024);
    watch_git_version_inputs(&repo);

    repo.head_commit()
        .ok()?
        .describe()
        .id_as_fallback(true)
        .try_resolve()
        .ok()??
        .format_with_dirty_suffix("dirty".to_owned())
        .ok()
        .map(|description| description.to_string())
}

#[cfg(feature = "git")]
fn watch_git_version_inputs(repo: &gix::Repository) {
    fn watch(path: impl AsRef<std::path::Path>) {
        let path = path.as_ref();
        // Cargo treats a missing watched file as always changed. Watch existing
        // ref directories as well so newly created loose refs are still seen.
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }

    watch(repo.git_dir().join("HEAD"));
    watch(repo.git_dir().join("config.worktree"));
    watch(repo.common_dir().join("config"));
    watch(repo.common_dir().join("packed-refs"));
    watch(repo.common_dir().join("refs"));
    watch(repo.common_dir().join("reftable"));
    watch(repo.index_path());

    // Track source changes for the dirty suffix without traversing untracked
    // build output, caches, or unrelated files in the working directory.
    if let (Some(worktree), Ok(index)) = (repo.workdir(), repo.index()) {
        for entry in index.entries() {
            watch(worktree.join(gix::path::from_bstr(entry.path(&index))));
        }
    }
}
