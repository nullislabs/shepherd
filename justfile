# Sync WIT deps (copies nexum-runtime into shepherd-cow/deps)
sync-wit:
    rm -rf wit/shepherd-cow/deps/nexum-runtime
    cp -r wit/nexum-runtime wit/shepherd-cow/deps/nexum-runtime

# Build the host runtime
build-runtime: sync-wit
    cargo build -p nexum-engine

# Build the example WASM module
build-module:
    cargo build --target wasm32-wasip2 --release -p example

# Build everything
build: build-runtime build-module

# Build the module then run the runtime with it
run: build-module build-runtime
    cargo run -p nexum-engine -- target/wasm32-wasip2/release/example.wasm

# Check the entire workspace
check: sync-wit
    cargo check --target wasm32-wasip2 -p example
    cargo check -p nexum-engine
