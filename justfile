# codex-ctl justfile
# Works on Linux and macOS

bin_name := "codex-ctl"
install_dir := env("HOME") / ".local" / "bin"

# Build release binary
build:
    cargo build --release

# Install to ~/.local/bin
install: build
    mkdir -p {{ install_dir }}
    cp target/release/{{ bin_name }} {{ install_dir }}/{{ bin_name }}
    @echo "Installed {{ bin_name }} to {{ install_dir }}/{{ bin_name }}"

# Uninstall from ~/.local/bin
uninstall:
    rm -f {{ install_dir }}/{{ bin_name }}
    @echo "Removed {{ install_dir }}/{{ bin_name }}"

# Run tests
test:
    cargo test

# Clean build artifacts
clean:
    cargo clean
