#![cfg(windows)]
use std::process::Command;

#[test]
fn substitutions_braces_and_slices_keep_fish_semantics() {
    let source = r#"
        set values one two three
        printf '<%s>\n' $values[-1..1]
        printf '<%s>\n' {one,two}{A,B}
        set total (math '4 + 5')
        echo total:$total
        set touched before
        echo (set touched after; echo captured)
        echo $touched
        printf '<%s>\n' (printf 'a\n\nb\n\n')
        printf '<%s>\n' "$(printf 'a\n\nb\n\n')"
        echo (exit 4)
        echo survived
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_fish"))
        .args(["-N", "-c", source])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "<three>\n<two>\n<one>\n<oneA>\n<twoA>\n<oneB>\n<twoB>\ntotal:9\ncaptured\nafter\n<a>\n<>\n<b>\n<>\n<a\n\nb>\n\nsurvived\n"
    );
}

#[test]
fn globs_use_native_paths_and_preserve_quoted_stars() {
    let root = std::env::temp_dir().join(format!("fish-native-glob-{}", std::process::id()));
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::write(root.join("a.txt"), b"a").unwrap();
    std::fs::write(root.join("b.txt"), b"b").unwrap();
    std::fs::write(root.join(".hidden.txt"), b"hidden").unwrap();
    std::fs::write(root.join("nested").join("c.txt"), b"c").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_fish")).current_dir(&root).args(["-N", "-c", "printf '<%s>\\n' *.txt; printf '<%s>\\n' **/*.txt; set literal '*.txt'; echo $literal; echo '*.txt'"]).output().unwrap();
    let resolved = root.canonicalize().unwrap();
    assert_eq!(
        resolved.parent().unwrap(),
        std::env::temp_dir().canonicalize().unwrap()
    );
    std::fs::remove_dir_all(&resolved).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "<a.txt>\n<b.txt>\n<a.txt>\n<b.txt>\n<nested/c.txt>\n*.txt\n*.txt\n"
    );
}

#[test]
fn nullglob_contexts_keep_empty_lists() {
    let output = Command::new(env!("CARGO_BIN_EXE_fish")).args(["-N", "-c", "set values ./no-native-fish-match-3948572394857-*; count $values; for value in ./no-native-fish-match-3948572394857-*; echo unexpected; end; count ./no-native-fish-match-3948572394857-*; echo survived"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"0\n0\nsurvived\n");
}
