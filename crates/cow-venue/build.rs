//! Turn the shipped `data/classification.toml` into a generated lookup
//! table at build time, so the runtime `client` slice (and any wasm
//! guest that links it) carries no TOML parser. The parse and the table
//! invariants live in `src/classification_data.rs`, shared verbatim with
//! the crate's parity tests.

#[path = "src/classification_data.rs"]
mod classification_data;

use std::{env, fs, path::Path};

use classification_data::{Action, parse_and_validate};

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let data = Path::new(&manifest).join("data/classification.toml");
    println!("cargo:rerun-if-changed={}", data.display());
    println!("cargo:rerun-if-changed=src/classification_data.rs");

    let toml = fs::read_to_string(&data).expect("read data/classification.toml");
    let entries =
        parse_and_validate(&toml).expect("shipped cow classification.toml is well formed");

    let mut out = String::new();
    out.push_str("// @generated from data/classification.toml by build.rs; do not edit.\n");
    out.push_str("static GENERATED_ROWS: &[GeneratedRow] = &[\n");
    for e in &entries {
        let action = match e.action {
            Action::TryNextBlock => "GenAction::TryNextBlock",
            Action::Backoff => "GenAction::Backoff",
            Action::Drop => "GenAction::Drop",
        };
        out.push_str(&format!(
            "    GeneratedRow {{ error_type: {:?}, action: {action}, \
             backoff_seconds: {}, already_submitted: {} }},\n",
            e.error_type, e.backoff_seconds, e.already_submitted,
        ));
    }
    out.push_str("];\n");

    let dest = Path::new(&env::var("OUT_DIR").expect("OUT_DIR")).join("classification_table.rs");
    fs::write(&dest, out).expect("write generated classification table");
}
