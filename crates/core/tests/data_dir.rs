//! `MM_DATA_DIR` moves the library. Alone in its own test binary because it
//! changes the process environment.

use mm_core::library::Library;

#[test]
fn data_dir_override() {
    let tmp = tempfile::tempdir().unwrap();
    // SAFETY: the only test in this binary, so no other thread reads the environment.
    unsafe { std::env::set_var("MM_DATA_DIR", tmp.path()) };
    assert_eq!(Library::default_root(), tmp.path());
    unsafe { std::env::set_var("MM_DATA_DIR", "") };
    assert_ne!(Library::default_root(), tmp.path());
}
