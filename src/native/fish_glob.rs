//! Native filesystem globbing; quoted and expanded wildcard characters stay literal.
use super::{Error, Item, Result, rendered};
use std::path::{Path, PathBuf};

pub(super) fn matches(pattern: &[Item], value: &str) -> bool {
    let value: Vec<_> = value.chars().collect();
    let (mut p, mut v, mut star, mut backtrack) = (0, 0, None, 0);
    while v < value.len() {
        if pattern.get(p) == Some(&Item::Literal(value[v])) {
            p += 1;
            v += 1;
        } else if pattern.get(p) == Some(&Item::Star) {
            star = Some(p);
            p += 1;
            backtrack = v;
        } else if let Some(star) = star {
            backtrack += 1;
            v = backtrack;
            p = star + 1;
        } else {
            return false;
        }
    }
    while pattern.get(p) == Some(&Item::Star) {
        p += 1;
    }
    p == pattern.len()
}

pub(super) fn expand(words: Vec<Vec<Item>>) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for word in words {
        if !word.contains(&Item::Star) {
            result.push(rendered(&word));
            continue;
        }
        let separator = word
            .iter()
            .find_map(|item| match item {
                Item::Literal(c @ ('/' | '\\')) => Some(*c),
                _ => None,
            })
            .unwrap_or('/');
        let components: Vec<Vec<Item>> = word
            .split(|item| matches!(item, Item::Literal('/' | '\\')))
            .map(<[_]>::to_vec)
            .collect();
        let first = components
            .iter()
            .position(|component| component.contains(&Item::Star))
            .unwrap();
        let mut prefix = components[..first]
            .iter()
            .map(|component| rendered(component))
            .collect::<Vec<_>>()
            .join(&separator.to_string());
        if first > 0 {
            prefix.push(separator);
        }
        let base = if prefix.is_empty() {
            PathBuf::from(".")
        } else {
            PathBuf::from(&prefix)
        };
        let mut matches = Vec::new();
        walk(
            &base,
            &prefix,
            &components[first..],
            separator,
            0,
            &mut matches,
        )?;
        matches.sort();
        matches.dedup();
        if matches.is_empty() {
            return Err(Error {
                message: format!("No matches for wildcard '{}'", rendered(&word)),
                status: 124,
            });
        }
        result.extend(matches);
        if result.len() > 100_000 {
            return Err(Error::new("too many wildcard matches"));
        }
    }
    Ok(result)
}

fn walk(
    base: &Path,
    display: &str,
    components: &[Vec<Item>],
    separator: char,
    depth: usize,
    output: &mut Vec<String>,
) -> Result<()> {
    if depth > 64 {
        return Err(Error::new("wildcard directory nesting limit exceeded"));
    }
    if components.is_empty() {
        output.push(display.trim_end_matches(separator).to_owned());
        return Ok(());
    }
    if components.len() == 1 && components[0].is_empty() {
        if base.is_dir() {
            output.push(display.to_owned());
        }
        return Ok(());
    }
    let component = &components[0];
    let recursive = component == &[Item::Star, Item::Star];
    if recursive && components.len() > 1 {
        walk(base, display, &components[1..], separator, depth, output)?;
    }
    let entries = match std::fs::read_dir(base) {
        Ok(entries) => entries,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::PermissionDenied
                    | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| Error::new("wildcard filename is not Unicode"))?;
        if name.starts_with('.') && component.first() != Some(&Item::Literal('.')) {
            continue;
        }
        if !matches(component, &name) {
            continue;
        }
        let path = entry.path();
        let shown = format!("{display}{name}");
        let file_type = entry.file_type()?;
        if components.len() == 1 {
            output.push(shown.clone());
        } else if file_type.is_dir() {
            walk(
                &path,
                &format!("{shown}{separator}"),
                &components[1..],
                separator,
                depth + 1,
                output,
            )?;
        }
        if recursive && file_type.is_dir() && !file_type.is_symlink() {
            walk(
                &path,
                &format!("{shown}{separator}"),
                components,
                separator,
                depth + 1,
                output,
            )?;
        }
        if output.len() > 100_000 {
            return Err(Error::new("too many wildcard matches"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wildcard_characters_in_values_are_literal() {
        assert_eq!(expand(vec![vec![Item::Literal('*')]]).unwrap(), ["*"]);
        assert!(matches(&[Item::Literal('a'), Item::Star], "alpha"));
        assert!(!matches(&[Item::Literal('a'), Item::Star], "beta"));
    }
}
