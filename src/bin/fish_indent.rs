#[cfg(not(windows))]
use fish::builtins::fish_indent;

#[cfg(not(windows))]
fn main() {
    fish_indent::main();
}

#[cfg(windows)]
fn main() {
    std::process::exit(fish::native::indent::main());
}
