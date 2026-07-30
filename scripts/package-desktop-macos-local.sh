#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/package-desktop-macos-local.sh \
  [--target native|all|aarch64-apple-darwin|x86_64-apple-darwin] \
  [--notary-profile PROFILE] \
  [--output-dir PATH] \
  [--skip-notarization]

Build Developer ID-signed Sigil desktop DMGs on the publisher's Mac. The
default target is the native architecture. Use --target all for a public
two-architecture release. Notarization, stapling, and Gatekeeper verification
are enabled by default.
USAGE
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

target_mode="native"
notary_profile="${SIGIL_NOTARY_PROFILE:-Sigil-Notary}"
output_root=""
skip_notarization=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)
      target_mode="${2-}"
      shift 2
      ;;
    --notary-profile)
      notary_profile="${2-}"
      shift 2
      ;;
    --output-dir)
      output_root="${2-}"
      shift 2
      ;;
    --skip-notarization)
      skip_notarization=true
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "Developer ID desktop packaging must run on macOS" >&2
  exit 1
fi
if [[ -n "$(git status --porcelain)" ]]; then
  echo "the worktree must be clean before producing signed desktop packages" >&2
  exit 1
fi

for command_name in git node pnpm rustup security xcrun codesign ditto shasum; do
  command -v "$command_name" >/dev/null || {
    echo "missing required command: $command_name" >&2
    exit 1
  }
done

native_target() {
  case "$(uname -m)" in
    arm64) echo "aarch64-apple-darwin" ;;
    x86_64) echo "x86_64-apple-darwin" ;;
    *)
      echo "unsupported macOS architecture: $(uname -m)" >&2
      return 1
      ;;
  esac
}

case "$target_mode" in
  native)
    targets=("$(native_target)")
    ;;
  all)
    targets=("aarch64-apple-darwin" "x86_64-apple-darwin")
    ;;
  aarch64-apple-darwin | x86_64-apple-darwin)
    targets=("$target_mode")
    ;;
  *)
    echo "unsupported target mode: $target_mode" >&2
    exit 2
    ;;
esac

identity="${APPLE_SIGNING_IDENTITY:-}"
if [[ -z "$identity" ]]; then
  identity="$(
    security find-identity -v -p codesigning |
      sed -nE 's/^.*"(Developer ID Application:[^"]+)".*$/\1/p' |
      head -n 1
  )"
fi
if [[ -z "$identity" || "$identity" == "-" ]] ||
  ! security find-identity -v -p codesigning | grep -Fq "\"$identity\""; then
  echo "a valid Developer ID Application identity is required in the login keychain" >&2
  exit 1
fi

team_id="${APPLE_TEAM_ID:-$(
  printf '%s\n' "$identity" |
    sed -nE 's/^.*\(([A-Z0-9]{10})\)$/\1/p'
)}"
if [[ ! "$team_id" =~ ^[A-Z0-9]{10}$ ]]; then
  echo "unable to derive a valid Apple Team ID from the signing identity" >&2
  exit 1
fi

if [[ "$skip_notarization" == false ]]; then
  xcrun notarytool history \
    --keychain-profile "$notary_profile" \
    --output-format json >/dev/null
fi

version="$(node -p "require('./apps/desktop/package.json').version")"
cargo_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
if [[ -z "$version" || "$version" != "$cargo_version" ]]; then
  echo "desktop and Cargo workspace versions must match before packaging" >&2
  exit 1
fi

commit="$(git rev-parse HEAD)"
if [[ -z "$output_root" ]]; then
  build_stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  output_root="$repo_root/.repo-local-dev/desktop-macos/$version/${commit:0:12}/$build_stamp"
elif [[ "$output_root" != /* ]]; then
  output_root="$repo_root/$output_root"
fi
if [[ -e "$output_root" ]]; then
  echo "output directory already exists: $output_root" >&2
  exit 1
fi
mkdir -p "$output_root"

pnpm --dir apps/desktop install --frozen-lockfile
rustup target add "${targets[@]}"

find_one_dmg() {
  local directory=$1
  local matches
  matches="$(find "$directory" -maxdepth 1 -type f -name '*.dmg' -print | sort)"
  if [[ "$(printf '%s\n' "$matches" | sed '/^$/d' | wc -l | tr -d ' ')" != "1" ]]; then
    echo "expected exactly one DMG in $directory; found:" >&2
    printf '%s\n' "$matches" >&2
    return 1
  fi
  printf '%s\n' "$matches"
}

for target in "${targets[@]}"; do
  case "$target" in
    aarch64-apple-darwin) expected_arch="arm64" ;;
    x86_64-apple-darwin) expected_arch="x86_64" ;;
  esac

  APPLE_SIGNING_IDENTITY="$identity" \
    pnpm --dir apps/desktop package \
      --target "$target" \
      --bundles app,dmg

  bundle_root="$repo_root/target/$target/release/bundle"
  source_dmg="$(find_one_dmg "$bundle_root/dmg")"
  output_dmg="$output_root/Sigil_${version}_${target}.dmg"
  ditto "$source_dmg" "$output_dmg"

  "$repo_root/scripts/verify-desktop-macos.sh" \
    --dmg "$output_dmg" \
    --expected-arch "$expected_arch" \
    --team-id "$team_id"

  if [[ "$skip_notarization" == false ]]; then
    "$repo_root/scripts/notarize-desktop-macos.sh" \
      --dmg "$output_dmg" \
      --expected-arch "$expected_arch" \
      --team-id "$team_id" \
      --notary-profile "$notary_profile"
  fi

  (
    cd "$output_root"
    shasum -a 256 "$(basename "$output_dmg")" >"$(basename "$output_dmg").sha256"
  )
done

cat >"$output_root/build.txt" <<EOF
version=$version
commit=$commit
team_id=$team_id
identity=$identity
targets=$(printf '%s ' "${targets[@]}")
notarized=$([[ "$skip_notarization" == false ]] && echo true || echo false)
EOF

echo "signed Sigil desktop packages are ready at $output_root"
