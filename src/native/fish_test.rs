//! Native filesystem and terminal predicates for the shared Fish test parser.
use super::*;
use crate::test_expression::{Context, FileComparison, FilePermission, StatPredicate, TestParser};
use fish_feature_flags::{FeatureFlag, feature_test};
use std::fs;
use std::path::Path;

struct NativeContext<'a, H> {
    host: &'a mut H,
    output: &'a Output,
}

impl<H: Host> Context for NativeContext<'_, H> {
    fn file_stat(
        &mut self,
        predicate: StatPredicate,
        path: &wstr,
    ) -> std::result::Result<bool, WString> {
        let path = path.to_string();
        let path = Path::new(&path);
        // Device handles need not have filesystem metadata.
        if predicate == StatPredicate::c {
            return Ok(is_character_device(path));
        }
        let Ok(metadata) = fs::metadata(path) else {
            return Ok(false);
        };
        Ok(match predicate {
            StatPredicate::d => metadata.is_dir(),
            StatPredicate::e => true,
            StatPredicate::f => metadata.is_file(),
            StatPredicate::s => metadata.len() > 0,
            StatPredicate::b | StatPredicate::c | StatPredicate::p | StatPredicate::S => false,
            // Native Windows has no POSIX effective uid/gid or set-id/sticky mode bits.
            StatPredicate::G
            | StatPredicate::g
            | StatPredicate::k
            | StatPredicate::O
            | StatPredicate::u => {
                let option = match predicate {
                    StatPredicate::G => "-G",
                    StatPredicate::g => "-g",
                    StatPredicate::k => "-k",
                    StatPredicate::O => "-O",
                    _ => "-u",
                };
                return Err(WString::from(format!(
                    "test: {option} requires POSIX ownership or mode bits"
                )));
            }
        })
    }

    fn file_permission(
        &mut self,
        permission: FilePermission,
        path: &wstr,
    ) -> std::result::Result<bool, WString> {
        {
            use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
            use windows_sys::Win32::Storage::FileSystem::FILE_EXECUTE;
            let access = match permission {
                FilePermission::r => GENERIC_READ,
                FilePermission::w => GENERIC_WRITE,
                FilePermission::x => FILE_EXECUTE,
            };
            Ok(open_for_access(Path::new(&path.to_string()), access).is_ok())
        }
    }

    fn is_symlink(&mut self, path: &wstr) -> std::result::Result<bool, WString> {
        Ok(fs::symlink_metadata(path.to_string()).is_ok_and(|metadata| metadata.is_symlink()))
    }

    fn compare_files(
        &mut self,
        comparison: FileComparison,
        left: &wstr,
        right: &wstr,
    ) -> std::result::Result<bool, WString> {
        let left = left.to_string();
        let right = right.to_string();
        let (left, right) = (Path::new(&left), Path::new(&right));
        Ok(match comparison {
            // A missing path never identifies an existing file, including when
            // both operands are missing. Compare volume and full file identity.
            FileComparison::Same => file_identity(left)
                .zip(file_identity(right))
                .is_some_and(|(left, right)| left == right),
            FileComparison::Newer | FileComparison::Older => {
                let left = fs::metadata(left)
                    .and_then(|metadata| metadata.modified())
                    .ok();
                let right = fs::metadata(right)
                    .and_then(|metadata| metadata.modified())
                    .ok();
                match comparison {
                    FileComparison::Newer => left > right,
                    _ => left < right,
                }
            }
        })
    }

    fn is_terminal(&mut self, fd: i32) -> std::result::Result<bool, WString> {
        Ok(self
            .host
            .fd_is_terminal(fd, &self.output.redirects, self.output.capture)
            .unwrap_or(false))
    }
}

pub(super) fn is_character_device(path: &Path) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_CHAR, GetFileType};
    open_for_access(path, 0).is_ok_and(|file| {
        // The live file owns this handle for the duration of the query.
        unsafe { GetFileType(file.as_raw_handle()) == FILE_TYPE_CHAR }
    })
}

/// Opening an existing handle probes Windows access without writing, creating,
/// or truncating anything. Share all access so the probe itself imposes no lock.
/// A conflicting existing sharing mode can still make the requested access fail.
pub(super) fn open_for_access(path: &Path, access: u32) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    fs::OpenOptions::new()
        .access_mode(access)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

fn file_identity(path: &Path) -> Option<(u64, [u8; 16])> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };
    let file = open_for_access(path, 0).ok()?;
    let mut info = FILE_ID_INFO::default();
    // This API returns the full 128-bit ID, including on ReFS. The buffer has
    // exactly the size and alignment required by FileIdInfo and the handle lives.
    let success = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    (success != 0).then_some((info.VolumeSerialNumber, info.FileId.Identifier))
}

impl Session {
    pub(super) fn test_builtin(
        &mut self,
        name: &str,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let args = if name == "[" {
            if args.last().map(String::as_str) != Some("]") {
                return self.test_diagnostic(
                    2,
                    &format!("{name}: the last argument must be ']'\n"),
                    host,
                    output,
                );
            }
            &args[..args.len() - 1]
        } else {
            args
        };
        if feature_test(FeatureFlag::TestRequireArg) {
            if args.is_empty() {
                return self.test_diagnostic(
                    2,
                    &format!("{name}: Expected at least one argument\n"),
                    host,
                    output,
                );
            }
            if args.len() == 1 && matches!(args[0].as_str(), "-n" | "-z") {
                self.status = i32::from(args[0] == "-n");
                return Ok(());
            }
        } else if args.len() <= 1 {
            self.status = args.first().map_or(2, |value| i32::from(value.is_empty()));
            return Ok(());
        }
        let args: Vec<WString> = args
            .iter()
            .map(|argument| WString::from(argument.as_str()))
            .collect();
        let expression = match TestParser::parse_args(&args) {
            Ok(expression) => expression,
            Err(error) => {
                return self.test_diagnostic(1, &format!("{name}: {error}"), host, output);
            }
        };
        let mut errors = Vec::new();
        let value = expression.evaluate(&mut NativeContext { host, output }, &mut errors);
        if !errors.is_empty() {
            let diagnostic = errors
                .into_iter()
                .map(|error| format!("{error}\n"))
                .collect::<String>();
            return self.test_diagnostic(2, &diagnostic, host, output);
        }
        self.status = i32::from(!value);
        Ok(())
    }

    fn test_diagnostic(
        &mut self,
        status: i32,
        message: &str,
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        self.status = status;
        output.append(host.write(2, message.as_bytes(), &output.redirects, output.capture)?)?;
        Ok(())
    }
}
