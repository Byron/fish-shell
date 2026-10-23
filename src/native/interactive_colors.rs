//! Validated Fish color specifications for the native Windows editor and prompts.
//!
//! This remains independent of POSIX terminal discovery. The `fish-color`
//! crate currently depends on `fish-common`'s POSIX platform services, so the
//! small color-name/RGB parser lives here until those dependencies can separate.

use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Color {
    Normal,
    Named(u8),
    Rgb(u8, u8, u8),
}

impl Color {
    fn parse(value: &str) -> Option<Self> {
        let value = value.to_ascii_lowercase();
        if value == "normal" {
            return Some(Self::Normal);
        }
        let (bright, name) = if value == "brown" {
            (false, "brown")
        } else {
            value
                .strip_prefix("br")
                .map_or((false, value.as_str()), |name| (true, name))
        };
        let index = match name {
            "black" => Some(0),
            "red" => Some(1),
            "green" => Some(2),
            "yellow" | "brown" => Some(3),
            "blue" => Some(4),
            "magenta" | "purple" => Some(5),
            "cyan" => Some(6),
            "white" => Some(7),
            "grey" | "gray" => Some(if bright { 0 } else { 7 }),
            _ => None,
        };
        if let Some(index) = index {
            return Some(Self::Named(index + if bright { 8 } else { 0 }));
        }
        let value = value.strip_prefix('#').unwrap_or(&value);
        if !matches!(value.len(), 3 | 6) || !value.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let bits = u32::from_str_radix(value, 16).ok()?;
        Some(if value.len() == 3 {
            Self::Rgb(
                ((bits >> 8) as u8 & 15) * 17,
                ((bits >> 4) as u8 & 15) * 17,
                (bits as u8 & 15) * 17,
            )
        } else {
            Self::Rgb((bits >> 16) as u8, (bits >> 8) as u8, bits as u8)
        })
    }

    fn sgr(self, background: bool) -> String {
        match self {
            Self::Normal => if background { "49" } else { "39" }.into(),
            Self::Named(index) => {
                let base = if background { 40 } else { 30 };
                (base + index % 8 + if index >= 8 { 60 } else { 0 }).to_string()
            }
            Self::Rgb(red, green, blue) => format!(
                "{};2;{red};{green};{blue}",
                if background { 48 } else { 38 }
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Style {
    foreground: Option<Color>,
    background: Option<Color>,
    bold: Option<bool>,
    dim: Option<bool>,
    italics: Option<bool>,
    underline: Option<bool>,
    reverse: Option<bool>,
    strikethrough: Option<bool>,
    reset: bool,
}

impl Style {
    fn foreground(color: Color) -> Self {
        Self {
            foreground: Some(color),
            ..Self::default()
        }
    }

    fn sgr(self, reset: bool, emphasize: bool) -> String {
        let mut codes = vec![];
        if reset || self.reset {
            codes.push("0".into());
        }
        if let Some(color) = self.foreground {
            codes.push(color.sgr(false));
        }
        if let Some(color) = self.background {
            codes.push(color.sgr(true));
        }
        for (enabled, on, off) in [
            (if emphasize { Some(true) } else { self.bold }, "1", "22"),
            (self.dim, "2", "22"),
            (self.italics, "3", "23"),
            (
                if emphasize {
                    Some(true)
                } else {
                    self.underline
                },
                "4",
                "24",
            ),
            (self.reverse, "7", "27"),
            (self.strikethrough, "9", "29"),
        ] {
            if let Some(enabled) = enabled {
                codes.push(if enabled { on } else { off }.into());
            }
        }
        format!("\x1b[{}m", codes.join(";"))
    }
}

/// A validated snapshot of Fish's color variables. Invalid values fall back to
/// the native defaults and are never copied into terminal output.
#[derive(Clone, Debug)]
pub(super) struct Palette {
    styles: BTreeMap<&'static str, Style>,
}

impl Default for Palette {
    fn default() -> Self {
        let normal = Style::default();
        let named = |index| Style::foreground(Color::Named(index));
        Self {
            styles: BTreeMap::from([
                ("fish_color_normal", normal),
                ("fish_color_param", normal),
                ("fish_color_option", normal),
                ("fish_color_command", named(4)),
                ("fish_color_keyword", named(4)),
                ("fish_color_quote", named(3)),
                ("fish_color_escape", named(6)),
                ("fish_color_operator", named(6)),
                ("fish_color_redirection", named(6)),
                ("fish_color_end", named(6)),
                ("fish_color_comment", named(8)),
                ("fish_color_error", named(9)),
                ("fish_color_autosuggestion", named(8)),
            ]),
        }
    }
}

impl Palette {
    pub(super) fn from_variables(variables: &BTreeMap<String, Vec<String>>) -> Self {
        let mut palette = Self::default();
        for (name, style) in &mut palette.styles {
            if let Some(arguments) = variables.get(*name)
                && let Ok(parsed) = parse_style(arguments.iter().map(String::as_str))
            {
                *style = parsed;
            }
        }
        // Fish's keyword and option colors inherit their parent role when
        // unset; an explicitly empty list instead requests ordinary text.
        for (name, fallback) in [
            ("fish_color_param", "fish_color_normal"),
            ("fish_color_keyword", "fish_color_command"),
            ("fish_color_option", "fish_color_param"),
        ] {
            if !variables.contains_key(name) {
                palette.styles.insert(name, palette.styles[fallback]);
            }
        }
        palette
    }

    pub(super) fn sequence(&self, variable: &str, emphasize: bool) -> String {
        self.styles
            .get(variable)
            .unwrap_or(&self.styles["fish_color_normal"])
            .sgr(true, emphasize)
    }

    pub(super) fn highlight_hint(&self, hint: &str) -> String {
        format!(
            "{}{hint}\x1b[0m",
            self.styles["fish_color_autosuggestion"].sgr(false, false)
        )
    }
}

/// Produce only validated SGR sequences for the native `set_color` builtin.
/// Arguments exclude the builtin name. No shell parsing or evaluation occurs.
pub(super) fn set_color(arguments: &[String]) -> Result<String, String> {
    if arguments.is_empty() {
        return Err("expected a color or style".into());
    }
    let style = parse_style(arguments.iter().map(String::as_str))?;
    if style == Style::default() {
        return Err("expected a color or style".into());
    }
    Ok(style.sgr(false, false))
}

fn parse_style<'a>(arguments: impl IntoIterator<Item = &'a str>) -> Result<Style, String> {
    let mut style = Style::default();
    let mut arguments = arguments.into_iter();
    let mut options = true;
    while let Some(argument) = arguments.next() {
        if argument.chars().any(char::is_control) {
            return Err("control characters are not valid colors or styles".into());
        }
        if options && argument == "--" {
            options = false;
            continue;
        }
        if options && argument.starts_with('-') {
            let (option, value) = argument
                .split_once('=')
                .map_or((argument, None), |(key, value)| (key, Some(value)));
            if matches!(option, "--background" | "-b" | "--foreground" | "-f") {
                let value = value
                    .or_else(|| arguments.next())
                    .ok_or_else(|| format!("{option} requires a color"))?;
                let color =
                    Color::parse(value).ok_or_else(|| format!("unknown color {value:?}"))?;
                let target = if matches!(option, "--background" | "-b") {
                    &mut style.background
                } else {
                    &mut style.foreground
                };
                if target.is_none() {
                    *target = Some(color);
                }
                continue;
            }
            let enabled = match value {
                None | Some("on") => Some(true),
                Some("single") if matches!(option, "--underline" | "-u") => Some(true),
                Some("off") => Some(false),
                _ => None,
            };
            match option {
                "--bold" | "-o" if value.is_none() => style.bold = Some(true),
                "--dim" | "-d" if value.is_none() => style.dim = Some(true),
                "--underline" | "-u" if enabled.is_some() => style.underline = enabled,
                "--italics" | "-i" if enabled.is_some() => style.italics = enabled,
                "--reverse" | "-r" if enabled.is_some() => style.reverse = enabled,
                "--strikethrough" | "-s" if enabled.is_some() => style.strikethrough = enabled,
                "--reset" if value.is_none() => style.reset = true,
                _ => return Err(format!("unknown color option {argument:?}")),
            }
        } else if argument == "reset" {
            style.reset = true;
        } else {
            let color =
                Color::parse(argument).ok_or_else(|| format!("unknown color {argument:?}"))?;
            if color == Color::Normal {
                style.reset = true;
            } else if style.foreground.is_none() {
                // Native Windows terminals support true color; the first color
                // is preferred and subsequent colors are compatibility fallbacks.
                style.foreground = Some(color);
            }
        }
    }
    Ok(style)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color(arguments: &[&str]) -> Result<String, String> {
        set_color(
            &arguments
                .iter()
                .map(|value| (*value).into())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn native_defaults_and_keyword_option_inheritance_are_stable() {
        let palette = Palette::default();
        assert_eq!(palette.sequence("fish_color_command", false), "\x1b[0;34m");
        assert_eq!(palette.sequence("fish_color_comment", false), "\x1b[0;90m");
        assert_eq!(palette.highlight_hint(" future"), "\x1b[90m future\x1b[0m");
        let variables = BTreeMap::from([
            (
                "fish_color_command".into(),
                vec!["magenta".into(), "--bold".into()],
            ),
            ("fish_color_param".into(), vec!["cyan".into()]),
        ]);
        let palette = Palette::from_variables(&variables);
        assert_eq!(
            palette.sequence("fish_color_keyword", false),
            "\x1b[0;35;1m"
        );
        assert_eq!(palette.sequence("fish_color_option", false), "\x1b[0;36m");
        let palette = Palette::from_variables(&BTreeMap::from([
            ("fish_color_command".into(), vec!["red".into()]),
            ("fish_color_keyword".into(), vec![]),
        ]));
        assert_eq!(palette.sequence("fish_color_keyword", false), "\x1b[0m");
    }

    #[test]
    fn named_rgb_and_background_colors_produce_only_sgr() {
        assert_eq!(color(&["BrCyAn"]).unwrap(), "\x1b[96m");
        assert_eq!(color(&["brown"]).unwrap(), "\x1b[33m");
        assert_eq!(color(&["brpurple"]).unwrap(), "\x1b[95m");
        assert_eq!(
            color(&["#1aF", "--background", "000000"]).unwrap(),
            "\x1b[38;2;17;170;255;48;2;0;0;0m"
        );
        assert_eq!(
            color(&["ff8040", "--background=brblue"]).unwrap(),
            "\x1b[38;2;255;128;64;104m"
        );
        assert_eq!(color(&["-b", "normal"]).unwrap(), "\x1b[49m");
        assert_eq!(color(&["-f", "green"]).unwrap(), "\x1b[32m");
        assert_eq!(color(&["abc", "red"]).unwrap(), "\x1b[38;2;170;187;204m");
    }

    #[test]
    fn text_attributes_and_normal_reset_work_for_prompt_construction() {
        assert_eq!(color(&["normal"]).unwrap(), "\x1b[0m");
        assert_eq!(color(&["--reset"]).unwrap(), "\x1b[0m");
        assert_eq!(
            color(&["green", "--bold", "--underline", "--italics", "--reverse"]).unwrap(),
            "\x1b[32;1;3;4;7m"
        );
        assert_eq!(
            color(&["--underline=off", "--italics=off", "--reverse=off"]).unwrap(),
            "\x1b[23;24;27m"
        );
        assert_eq!(color(&["-o", "-u", "-i", "-r"]).unwrap(), "\x1b[1;3;4;7m");
        let palette = Palette::from_variables(&BTreeMap::from([(
            "fish_color_operator".into(),
            vec!["red".into(), "--underline=off".into()],
        )]));
        assert_eq!(
            palette.sequence("fish_color_operator", true),
            "\x1b[0;31;1;4m"
        );
    }

    #[test]
    fn invalid_or_injected_values_never_reach_terminal_output() {
        for arguments in [
            vec![],
            vec![""],
            vec!["--"],
            vec!["not-a-color"],
            vec!["--background"],
            vec!["--bold=off"],
            vec!["--italics=single"],
            vec!["--eval", "echo bad"],
            vec!["#12345"],
            vec!["#ggg"],
            vec!["red\x1b]0;title\x07"],
            vec!["--background=\x1b[31m"],
            vec!["--background", "\x1b[31m"],
            vec!["red\nblue"],
        ] {
            assert!(color(&arguments).is_err(), "{arguments:?}");
        }
        let palette = Palette::from_variables(&BTreeMap::from([(
            "fish_color_command".into(),
            vec!["\x1b]0;injected\x07".into()],
        )]));
        assert_eq!(palette.sequence("fish_color_command", false), "\x1b[0;34m");
        assert_eq!(palette.sequence("fish_color_keyword", false), "\x1b[0;34m");
    }
}
