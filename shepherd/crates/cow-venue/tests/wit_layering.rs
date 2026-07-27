//! Layering gate: no generic WIT package references `shepherd:cow`.
//! The bundle-layer package carries only the event ABIs; the generic
//! host and videre packages this group vendors into `wit/deps/` must
//! never name it.

use std::path::Path;

#[test]
fn generic_wit_packages_never_reference_shepherd_cow() {
    let deps = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wit/deps");
    for pkg in std::fs::read_dir(&deps).expect("wit/deps dir") {
        let pkg = pkg.expect("wit/deps entry").path();
        for file in std::fs::read_dir(&pkg).expect("wit package dir") {
            let path = file.expect("wit package entry").path();
            let text = std::fs::read_to_string(&path).expect("read wit file");
            assert!(
                !text.contains("shepherd:cow"),
                "{} references shepherd:cow",
                path.display(),
            );
        }
    }
}
