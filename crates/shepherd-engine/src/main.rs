//! The `shepherd` binary: the thin entry point over the composition root
//! in `shepherd_engine`.

use shepherd_engine::ShepherdRuntime;

#[tokio::main]
async fn main() -> Result<(), nexum_launch::RunError> {
    nexum_launch::run("shepherd", ShepherdRuntime).await
}
