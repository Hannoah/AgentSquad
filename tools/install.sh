#!/usr/bin/env bash
# Installs squad from GitHub Releases (macOS).
#   curl -fsSL https://raw.githubusercontent.com/Hannoah/AgentSquad/main/tools/install.sh | bash
# Env: SQUAD_VERSION (e.g. v0.15.3, default latest), SQUAD_INSTALL_DIR (default ~/.local/bin),
#      SQUAD_REPO (default Hannoah/AgentSquad).
set -euo pipefail

repo="${SQUAD_REPO:-Hannoah/AgentSquad}"
version="${SQUAD_VERSION:-latest}"
dir="${SQUAD_INSTALL_DIR:-$HOME/.local/bin}"

[ "$(uname -s)" = "Darwin" ] || { echo "This installer is for macOS. On Windows use tools/install.ps1." >&2; exit 1; }
case "$(uname -m)" in
  arm64) asset="squad-macos-aarch64.tar.gz" ;;
  x86_64) asset="squad-macos-x86_64.tar.gz" ;;
  *) echo "Unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

if [ "$version" = "latest" ]; then
  base="https://github.com/$repo/releases/latest/download"
else
  base="https://github.com/$repo/releases/download/${version}"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "Downloading $asset ($version)..."
curl -fsSL "$base/$asset" -o "$tmp/$asset"
curl -fsSL "$base/$asset.sha256" -o "$tmp/$asset.sha256"

# Fail closed: nothing is installed unless the checksum matches.
expected="$(awk '{print tolower($1)}' "$tmp/$asset.sha256")"
actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
[[ "$expected" =~ ^[0-9a-f]{64}$ ]] || { echo "Checksum file is malformed; refusing to install." >&2; exit 1; }
[ "$expected" = "$actual" ] || { echo "SHA256 mismatch: expected $expected, got $actual" >&2; exit 1; }
echo "SHA256 verified: $actual"

mkdir -p "$dir"
tar -xzf "$tmp/$asset" -C "$tmp" squad
install -m 755 "$tmp/squad" "$dir/squad"
echo "Installed $("$dir/squad" --version) to $dir/squad"

case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "Add it to your PATH:  echo 'export PATH=\"$dir:\$PATH\"' >> ~/.zshrc && source ~/.zshrc" ;;
esac
