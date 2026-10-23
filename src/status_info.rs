//! Build, feature and embedded-file metadata shared by both execution backends.
use fish_feature_flags::{FeatureFlag, METADATA};
use fish_widestring::{WString, wstr};
use rust_embed::RustEmbed;
use std::borrow::Cow;
use std::fmt::Write as _;

struct EmptyEmbed;
impl RustEmbed for EmptyEmbed {
    fn get(_: &str) -> Option<rust_embed::EmbeddedFile> {
        None
    }
    fn iter() -> impl Iterator<Item = Cow<'static, str>> {
        std::iter::empty()
    }
}

#[cfg(all(not(windows), feature = "embed-manpages"))]
#[derive(RustEmbed)]
#[folder = "$FISH_RESOLVED_BUILD_DIR/fish-docs/man/man1"]
#[prefix = "man/man1/"]
struct Docs;
#[cfg(any(windows, not(feature = "embed-manpages")))]
type Docs = EmptyEmbed;

#[cfg(using_cmake)]
#[derive(RustEmbed)]
#[folder = "$FISH_CMAKE_BINARY_DIR/share"]
#[include = "__fish_build_paths.fish"]
struct CMakeBinaryDir;
#[cfg(not(using_cmake))]
type CMakeBinaryDir = EmptyEmbed;

pub(crate) fn get_file(path: &str) -> Option<Cow<'static, [u8]>> {
    crate::autoload::Asset::get(path)
        .or_else(|| Docs::get(path))
        .or_else(|| CMakeBinaryDir::get(path))
        .map(|file| file.data)
}

pub(crate) fn list_files(prefixes: &[&wstr]) -> Vec<WString> {
    let mut paths = Vec::new();
    let mut add = |prefix: &str| {
        for path in crate::autoload::Asset::iter()
            .chain(Docs::iter())
            .chain(CMakeBinaryDir::iter())
        {
            if path.starts_with(prefix) {
                paths.push(WString::from(path.as_ref()));
            }
        }
    };
    if prefixes.is_empty() {
        add("");
    } else {
        for prefix in prefixes {
            add(&prefix.to_string());
        }
    }
    paths.sort_by(|a, b| fish_util::wcsfilecmp_glob(a, b));
    paths
}

pub(crate) fn features(mut enabled: impl FnMut(FeatureFlag) -> bool) -> WString {
    let width = METADATA
        .iter()
        .map(|metadata| metadata.name.len())
        .max()
        .unwrap_or(0)
        + 1;
    let mut output = String::new();
    for metadata in METADATA {
        writeln!(
            output,
            "{:<width$}{:<3} {} {}",
            metadata.name.to_string(),
            if enabled(metadata.flag) { "on" } else { "off" },
            metadata.groups,
            metadata.description
        )
        .unwrap();
    }
    WString::from(output)
}

pub(crate) fn build_info() -> WString {
    let target = env!("BUILD_TARGET_TRIPLE");
    let host = env!("BUILD_HOST_TRIPLE");
    let mut output = format!(
        "Build system: {}\nVersion: {}\n",
        if cfg!(using_cmake) { "CMake" } else { "Cargo" },
        crate::BUILD_VERSION
    );
    if target == host {
        writeln!(output, "Target (and host): {target}").unwrap();
    } else {
        writeln!(output, "Target: {target}\nHost: {host}").unwrap();
    }
    let features: &[&str] = &[
        #[cfg(feature = "embed-manpages")]
        "embed-manpages",
        #[cfg(feature = "localize-messages")]
        "localize-messages",
        #[cfg(target_feature = "crt-static")]
        "crt-static",
    ];
    writeln!(
        output,
        "Profile: {}\nFeatures: {}\n",
        env!("BUILD_PROFILE"),
        features.join(" ")
    )
    .unwrap();
    WString::from(output)
}
