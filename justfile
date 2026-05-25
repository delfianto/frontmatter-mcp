bin     := "front"
bin_dir := env_var("HOME") / ".local/bin"

# List available recipes
default:
    @just --list

# Build release binary
build:
    cargo build --release

# Run all tests
test:
    cargo test

# Run tests with output (useful for debugging)
test-verbose:
    cargo test -- --nocapture

# Lint — treat warnings as errors
lint:
    cargo clippy -- -D warnings

# Build, test, and lint in one shot
check: build test lint

# Install front + front-mcp symlink into ~/.local/bin
install: build
    install -Dm755 target/release/{{bin}} {{bin_dir}}/{{bin}}
    ln -sf {{bin}} {{bin_dir}}/{{bin}}-mcp
    @echo "installed {{bin_dir}}/{{bin}}"
    @echo "symlinked {{bin_dir}}/{{bin}}-mcp → {{bin}}"

# Remove installed binaries
uninstall:
    rm -f {{bin_dir}}/{{bin}} {{bin_dir}}/{{bin}}-mcp
    @echo "removed {{bin_dir}}/{{bin}} and {{bin_dir}}/{{bin}}-mcp"

# Remove build artifacts
clean:
    cargo clean
