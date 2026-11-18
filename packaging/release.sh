#!/bin/sh
# One version of slimfit, cut on this Mac: factorium.toml's cut step runs
# `packaging/release.sh cut $FACTORIUM_VERSION` in a worktree at the base's
# tip, then factorium tags what it left at HEAD and `brew upgrade`s the
# formula.
#
#   release.sh cut <vX.Y.Z>   set the version and commit it, build for Apple
#                             Silicon, check `slimfit --version`, package,
#                             and publish: the archive to the tap's release
#                             slimfit-v<X.Y.Z>, the formula to the tap's
#                             Formula/slimfit.rb
#   release.sh check-tag <vX.Y.Z>  exit 0 if cut would take the version, 2 if not
#   release.sh dry            what the gate runs: package the binary the gate
#                             built (target/release/slimfit) at Cargo.toml's
#                             version, check it and the formula, publish
#                             nothing, commit nothing
#
# The archive is downloaded from the tap (sunfmin/homebrew-tap), as every
# formula there is. DIST names another directory than dist/.
set -eu
cd "$(dirname "$0")/.."
PATH="$PATH:$HOME/.cargo/bin"
export PATH

REPO=https://github.com/sunfmin/slimfit
TAP=sunfmin/homebrew-tap
TARGET=aarch64-apple-darwin
DIST=${DIST:-dist}

usage() {
    echo "usage: release.sh cut <vX.Y.Z> | dry | check-tag <vX.Y.Z>" >&2
    exit 2
}

cargo_version() {
    cargo pkgid -p slimfit | sed 's/.*[#@]//'
}

# check_binary <path> <version>: the binary says the version being cut.
check_binary() {
    said=$("$1" --version)
    [ "$said" = "slimfit $2" ] || {
        echo "release: $1 --version says '$said', not 'slimfit $2'" >&2
        exit 1
    }
}

# package <binary> <version>: the archive and the formula, in $DIST.
package() {
    mkdir -p "$DIST"
    archive=slimfit-$2-macos-arm64.tar.gz
    tar -czf "$DIST/$archive" -C "$(dirname "$1")" slimfit
    sha=$(shasum -a 256 "$DIST/$archive" | cut -d' ' -f1)
    release=slimfit-v$2
    formula=$DIST/slimfit.rb
    cat > "$formula" <<FORMULA
class Slimfit < Formula
  desc "Keep a codebase small and its tests few, fast and strong"
  homepage "$REPO"
  url "https://github.com/$TAP/releases/download/$release/$archive"
  sha256 "$sha"
  version "$2"

  depends_on :macos
  depends_on arch: :arm64

  def install
    bin.install "slimfit"
  end

  test do
    assert_equal "slimfit #{version}", shell_output("#{bin}/slimfit --version").strip
  end
end
FORMULA
    ruby -c "$formula" >/dev/null
}

# check_tag <tag>: a version tag vX.Y.Z, or usage.
check_tag() {
    case "$1" in
    v*[!0-9.]* | v*..* | v.* | v*.) usage ;;
    v[0-9]*.[0-9]*.[0-9]*) ;;
    *) usage ;;
    esac
    case "$1" in v*.*.*.*) usage ;; esac
}

case "${1:-}" in
check-tag)
    [ $# -eq 2 ] || usage
    check_tag "$2"
    exit 0
    ;;
dry)
    [ $# -eq 1 ] || usage
    version=$(cargo_version)
    built=target/release/slimfit
    check_binary "$built" "$version"
    package "$built" "$version"
    echo "release: dry ok (slimfit $version, $DIST/$archive)"
    exit 0
    ;;
cut) [ $# -eq 2 ] || usage ;;
*) usage ;;
esac
check_tag "$2"
version=${2#v}

# The version, in Cargo.toml and Cargo.lock, committed: the tag goes on it.
sed -i.bak "s/^version = \".*\"\$/version = \"$version\"/" Cargo.toml
rm Cargo.toml.bak
cargo update --quiet --workspace
said=$(cargo_version)
[ "$said" = "$version" ] || {
    echo "release: Cargo.toml says $said, not $version" >&2
    exit 1
}
if ! git diff --quiet; then
    git commit --quiet -am "slimfit $version"
fi

cargo build --release --locked -p slimfit --target "$TARGET"
built=target/$TARGET/release/slimfit
check_binary "$built" "$version"
package "$built" "$version"

# A release that is there already takes the archive again: a cut that
# failed half way is run a second time.
if gh release view "$release" -R "$TAP" >/dev/null 2>&1; then
    gh release upload "$release" -R "$TAP" --clobber "$DIST/$archive"
else
    gh release create "$release" -R "$TAP" --title "slimfit $version" \
        --notes "The binary the formula \`slimfit\` installs." "$DIST/$archive"
fi
tap=$(mktemp -d)
gh repo clone "$TAP" "$tap" -- --quiet --depth 1
cp "$formula" "$tap/Formula/slimfit.rb"
git -C "$tap" add Formula/slimfit.rb
if git -C "$tap" diff --cached --quiet; then
    echo "release: the tap's formula is slimfit $version already"
else
    git -C "$tap" commit --quiet -m "slimfit $version"
    git -C "$tap" push --quiet origin HEAD
fi
rm -rf "$tap"
