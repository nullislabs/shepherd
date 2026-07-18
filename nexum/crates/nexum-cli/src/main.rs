//! The bare `nexum` engine binary: the core lattice with no extension
//! payload, composed over the generic launcher.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use nexum_runtime::preset::CoreRuntime;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    nexum_launch::run("nexum", CoreRuntime).await
}
