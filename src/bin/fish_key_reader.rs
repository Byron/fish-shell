#[cfg(not(windows))]
use fish::builtins::fish_key_reader;

#[cfg(not(windows))]
fn main() {
    fish_key_reader::main();
}

#[cfg(windows)]
fn main() {
    std::process::exit(fish::native::key_reader::main());
}
