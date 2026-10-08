//! Error messages of `#[derive(View)]`.

#[test]
fn derive_errors() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}
