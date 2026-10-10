//! What users see from rustc for misused `#[browser_test]`s: the attribute's own errors, and the
//! type errors of the code it generates. Regenerate the expected output with
//! `TRYBUILD=overwrite cargo test --package browser-test-macros --test ui`.

#[test]
fn ui() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/*.rs");
}
