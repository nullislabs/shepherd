//! Layering gate: no vendored generic WIT package references
//! `shepherd:cow`. The bundle-layer package carries only the event
//! ABIs; the vendored host and videre packages must never name it.

use std::path::Path;

#[test]
fn vendored_wit_packages_never_reference_shepherd_cow() {
    let deps_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wit/deps");
    let mut seen = 0usize;
    for pkg in std::fs::read_dir(&deps_root).expect("wit/deps dir") {
        let pkg = pkg.expect("wit/deps entry").path();
        for file in std::fs::read_dir(&pkg).expect("wit package dir") {
            let path = file.expect("wit package entry").path();
            let text = std::fs::read_to_string(&path).expect("read wit file");
            assert!(
                !text.contains("shepherd:cow"),
                "{} references shepherd:cow",
                path.display(),
            );
            seen += 1;
        }
    }
    assert!(
        seen > 0,
        "no vendored wit files under {}",
        deps_root.display()
    );
}
