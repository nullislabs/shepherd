# Build the host engine
build-engine:
    cargo build -p nexum-engine

# Build the example WASM module
build-module:
    cargo build --target wasm32-wasip2 --release -p example

# Build everything
build: build-engine build-module

# Build the module then run the engine with it. The second argument is the
# module's nexum.toml — without it the engine prints the 0.1-compat
# deprecation warning and proceeds with empty capabilities/config.
run: build-module build-engine
    cargo run -p nexum-engine -- target/wasm32-wasip2/release/example.wasm modules/example/nexum.toml

# Check the entire workspace
check:
    cargo check --target wasm32-wasip2 -p example
    cargo check -p nexum-engine
