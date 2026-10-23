//! Numeric parsing and math use the same platform-independent code as POSIX Fish.
#[path = "../tinyexpr.rs"]
mod tinyexpr;

pub(super) fn math(args: &[String]) -> rash::Result<Vec<u8>> {
    let mut scale = 6usize;
    let mut base = 10;
    let mut mode = "default";
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--" => {
                index += 1;
                break;
            }
            "-s" | "--scale" | "-b" | "--base" | "-m" | "--scale-mode" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| rash::Error::new("math: missing option argument"))?;
                match arg.as_str() {
                    "-s" | "--scale" => {
                        scale = if value == "max" {
                            15
                        } else {
                            value
                                .parse()
                                .map_err(|_| rash::Error::new("math: invalid scale"))?
                        };
                        if scale > 15 {
                            return Err(rash::Error::new("math: scale must be between 0 and 15"));
                        }
                    }
                    "-b" | "--base" => {
                        base = match value.as_str() {
                            "hex" | "16" => 16,
                            "octal" | "8" => 8,
                            "10" => 10,
                            _ => return Err(rash::Error::new("math: base must be 8, 10, or 16")),
                        };
                    }
                    _ => {
                        mode = value;
                        if !matches!(
                            mode,
                            "truncate" | "trunc" | "round" | "floor" | "ceiling" | "ceil"
                        ) {
                            return Err(rash::Error::new("math: invalid scale mode"));
                        }
                    }
                }
                index += 2;
            }
            _ => break,
        }
    }
    if index == args.len() {
        return Err(rash::Error::new("math: expected an expression"));
    }
    let source = fish_widestring::WString::from(args[index..].join(" "));
    let mut value = tinyexpr::te_interp(&source).map_err(|error| {
        rash::Error::new(format!(
            "math: {} at character {}",
            error.kind.describe_wstr(),
            error.position + 1
        ))
    })?;
    if !value.is_finite() || value.abs() >= (1u64 << 53) as f64 {
        return Err(rash::Error::new(
            "math: result is not a finite, precisely representable number",
        ));
    }
    let rendered = if base != 10 {
        let sign = if value.is_sign_negative() { "-" } else { "" };
        let number = value.abs().trunc() as u64;
        if base == 16 {
            format!("{sign}0x{number:x}")
        } else {
            format!("{sign}0{number:o}")
        }
    } else {
        let factor = 10f64.powi(scale as i32);
        value *= factor;
        value = match mode {
            "truncate" | "trunc" => value.trunc(),
            "round" => value.round(),
            "floor" => value.floor(),
            "ceiling" | "ceil" => value.ceil(),
            _ if scale == 0 => value.trunc(),
            _ => value,
        };
        value /= factor;
        let rendered = format!("{value:.scale$}");
        if scale > 0 {
            rendered
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_owned()
        } else {
            rendered
        }
    };
    Ok(format!("{rendered}\n").into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_fish_math() {
        for (input, expected) in [
            (vec!["2 + 3 * 4"], "14\n"),
            (vec!["sin(pi / 2)"], "1\n"),
            (vec!["-s", "0", "7 / 2"], "3\n"),
            (vec!["-b", "hex", "255"], "0xff\n"),
        ] {
            let args: Vec<_> = input.into_iter().map(str::to_owned).collect();
            assert_eq!(math(&args).unwrap(), expected.as_bytes());
        }
        assert!(math(&["1 / 0".into()]).is_err());
    }
}
