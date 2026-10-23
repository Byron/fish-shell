use super::prelude::*;

/// Private read-only queries used by the bundled Git functions and completions.
pub fn git(parser: &mut Parser, streams: &mut IoStreams, args: &mut [&wstr]) -> BuiltinResult {
    if args.get(1) == Some(&L!("--is-enabled")) {
        return if cfg!(feature = "git") {
            Ok(SUCCESS)
        } else {
            Err(STATUS_CMD_ERROR)
        };
    }
    #[cfg(not(feature = "git"))]
    {
        let _ = (parser, streams);
        // Ask the shell helper to use the Git executable instead.
        Err(125)
    }
    #[cfg(feature = "git")]
    {
        use crate::env::Environment as _;
        use fish_widestring::{bytes2wcstring, wcs2bytes, wcs2osstring};
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;
        // shortcut: accept byte arguments when extending the private query protocol.
        let arguments = args[1..]
            .iter()
            .map(|arg| String::from_utf8(wcs2bytes(*arg)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| STATUS_INVALID_ARGS)?;
        let environment = parser
            .vars()
            .export_array()
            .iter()
            .filter_map(|entry| {
                let bytes = entry.as_bytes();
                let split = bytes.iter().position(|byte| *byte == b'=')?;
                Some((
                    OsStr::from_bytes(&bytes[..split]).to_owned(),
                    OsStr::from_bytes(&bytes[split + 1..]).to_owned(),
                ))
            })
            .collect::<Vec<_>>();
        let cwd = wcs2osstring(&parser.vars().get_pwd_slash());
        let output =
            crate::git::query::query(std::path::Path::new(&cwd), &environment, &arguments)?;
        streams.out.append(&bytes2wcstring(&output));
        Ok(SUCCESS)
    }
}
