use fish_widestring::WString;
use std::process::Command;

fn evaluate(source: &str) -> String {
    let translated = fish::bash_to_fish::translate(&WString::from(source)).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_fish"))
        .args(["-N", "-c", &translated.to_string()])
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    assert!(result.stderr.is_empty(), "{result:?}");
    String::from_utf8(result.stdout).unwrap()
}

#[test]
fn nested_defaults_handle_unset_empty_and_populated_windows_variables() {
    assert_eq!(
        evaluate(
            r#"
        set -e SYSTEMROOT
        set -e SystemRoot
        local_appdata="$LOCALAPPDATA"
        system_root="${SYSTEMROOT:-${SystemRoot:-C:/Windows}}"
        gix=./target/debug/gix.exe
        printf '<%s>\n' "$system_root" "$gix"
        SystemRoot='D:/Windows folder'
        printf '<%s>\n' "${SYSTEMROOT:-${SystemRoot:-C:/Windows}}"
        SYSTEMROOT=''
        printf '<%s>\n' "${SYSTEMROOT:-${SystemRoot:-C:/Windows}}" "${SYSTEMROOT-fallback}"
        SYSTEMROOT='E:/Windows'
        printf '<%s>\n' "${SYSTEMROOT:-${SystemRoot:-C:/Windows}}"
    "#
        ),
        "<C:/Windows>\n<./target/debug/gix.exe>\n<D:/Windows folder>\n<D:/Windows folder>\n<>\n<E:/Windows>\n"
    );
}

#[test]
fn defaults_and_alternates_expand_only_the_selected_operand() {
    assert_eq!(
        evaluate(
            r#"
        set -e absent
        full=value
        empty=''
        printf '<%s>\n' "${full:-$(echo wrong >&2)}" "${absent:+$(echo wrong >&2)}"
        printf '<%s>\n' "${empty+yes}" "${empty:+no}" "${absent-fallback}" "${full:+${absent:-nested value}}"
        printf '<%s>\n' ${absent:-two words} "${absent:-'quoted'}" "${absent:-"double quoted"}"
        printf '<%s>\n' ${absent:-a; echo literal} ${absent:-#literal|&<>}
    "#
        ),
        "<value>\n<>\n<yes>\n<>\n<fallback>\n<nested value>\n<two words>\n<'quoted'>\n<double quoted>\n<a; echo literal>\n<#literal|&<>>\n"
    );
}

#[test]
fn parameter_values_preserve_newlines_and_fish_list_elements() {
    assert_eq!(
        evaluate(
            "set -e absent; value='line\n'; printf '<%s>' \"${value:-wrong}\"; set items one 'two words'; printf '<%s>' ${items:-wrong}; printf '<%s>' \"${absent:-line\n}\""
        ),
        "<line\n><one><two words><line\n>"
    );
}
