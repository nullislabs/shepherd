# Sync WIT deps (copies nexum-host into shepherd-cow/deps)
sync-wit:
    rm -rf wit/shepherd-cow/deps/nexum-host
    cp -r wit/nexum-host wit/shepherd-cow/deps/nexum-host

# Build the host runtime
build-runtime: sync-wit
    cargo build -p nexum-engine

# Build the example WASM module
build-module:
    cargo build --target wasm32-wasip2 --release -p example

# Build everything
build: build-runtime build-module

# Build the module then run the runtime with it. The second argument is the
# module's nexum.toml — without it the engine prints the 0.1-compat
# deprecation warning and proceeds with empty capabilities/config.
run: build-module build-runtime
    cargo run -p nexum-engine -- target/wasm32-wasip2/release/example.wasm modules/example/nexum.toml

# Check the entire workspace
check: sync-wit
    cargo check --target wasm32-wasip2 -p example
    cargo check -p nexum-engine
