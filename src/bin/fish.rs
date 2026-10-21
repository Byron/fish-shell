#[cfg(not(windows))]
include!("fish/unix.rs");

#[cfg(windows)]
fn main() {
    std::process::exit(fish::native::main(fish::native::Language::Fish));
}
