bin     := "frontmatter"
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

# Compress the release binary with upx (skips if already packed)
compress: build
    upx -t target/release/{{bin}} >/dev/null 2>&1 || upx --best --lzma target/release/{{bin}}

# Install frontmatter into ~/.local/bin
install: compress
    install -Dm755 target/release/{{bin}} {{bin_dir}}/{{bin}}
    @echo "installed {{bin_dir}}/{{bin}}"

# Remove installed binary
uninstall:
    rm -f {{bin_dir}}/{{bin}}
    @echo "removed {{bin_dir}}/{{bin}}"

# Remove build artifacts
clean:
    cargo clean
