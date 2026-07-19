bin     := "frontmatter"
bin_dir := env_var("HOME") / ".local/bin"
sys_dir := "/usr/local/bin"

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

# Install frontmatter into ~/.local/bin (pass --system for /usr/local/bin via sudo)
install *flags: compress
    #!/usr/bin/env bash
    set -euo pipefail
    dir="{{bin_dir}}"
    sudo=""
    for f in {{flags}}; do
        case "$f" in
            --system) dir="{{sys_dir}}"; sudo="sudo" ;;
            *) echo "install: unknown flag '$f' (only --system is supported)" >&2; exit 1 ;;
        esac
    done
    $sudo install -Dm755 target/release/{{bin}} "$dir/{{bin}}"
    echo "installed $dir/{{bin}}"
    link="$dir/{{bin}}-mcp"
    if [ ! -e "$link" ] && [ ! -L "$link" ]; then
        $sudo ln -s "{{bin}}" "$link"   # relative target: resolves to sibling {{bin}}
        echo "linked $link -> {{bin}}"
    fi

# Remove installed binary + symlink (pass --system for /usr/local/bin via sudo)
uninstall *flags:
    #!/usr/bin/env bash
    set -euo pipefail
    dir="{{bin_dir}}"
    sudo=""
    for f in {{flags}}; do
        case "$f" in
            --system) dir="{{sys_dir}}"; sudo="sudo" ;;
            *) echo "uninstall: unknown flag '$f' (only --system is supported)" >&2; exit 1 ;;
        esac
    done
    $sudo rm -f "$dir/{{bin}}" "$dir/{{bin}}-mcp"
    echo "removed $dir/{{bin}} and $dir/{{bin}}-mcp"

# Remove build artifacts
clean:
    cargo clean
