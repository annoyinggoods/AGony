#!/usr/bin/env bash
# Package a release archive for the current platform (Part12 12.4.1):
# build `agony` in release mode and archive the binary with a short
# install note. Runs on all three platforms (bash: Git Bash on Windows).
# carapace acquisition stays scripted (scripts/get-carapace.ps1, Part03)
# — never vendored into the archive.
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' crates/agony/Cargo.toml | head -n 1)
case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*)
        triple=x86_64-pc-windows-msvc
        binary=agony.exe
        ;;
    Darwin)
        triple=aarch64-apple-darwin
        binary=agony
        ;;
    *)
        triple=x86_64-unknown-linux-gnu
        binary=agony
        ;;
esac

cargo build --release -p agony

stage="dist/agony-$version-$triple"
rm -rf "$stage"
mkdir -p "$stage"
cp "target/release/$binary" "$stage/"
# The license travels with the binary.
cp LICENSE "$stage/LICENSE"
cat > "$stage/INSTALL.txt" <<'EOF'
AGony — a desktop environment that lives in a terminal
Licensed under the Annoying Goods Open Source License with Ethical Restrictions — see the LICENSE file beside this note.

Run the binary in a terminal: `agony` starts the desktop; it needs the
rmux engine (`cargo install rmux`) and, for command completions, the
carapace binary — on Windows: powershell -ExecutionPolicy Bypass -File
scripts/get-carapace.ps1 from the repository; elsewhere install
carapace-bin and set AGONY_CARAPACE to it.

First run: `agony doctor` reports the environment and `agony selftest`
proves the hosting chain. See the repository README for the tour.
EOF
tar -czf "$stage.tar.gz" -C "$stage" .
echo "packaged: $stage.tar.gz"
