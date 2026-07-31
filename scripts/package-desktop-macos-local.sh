#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/package-desktop-macos-local.sh \
  [--target native|all|aarch64-apple-darwin|x86_64-apple-darwin] \
  [--notary-profile PROFILE] \
  [--updater-key PATH] \
  [--output-dir PATH] \
  [--skip-notarization]

Build Developer ID-signed Sigil desktop DMGs and Tauri-signed update archives
on the publisher's Mac. The default target is the native architecture. Use
--target all for a public two-architecture release. Notarization, stapling,
Gatekeeper verification, and updater archive signing are enabled by default.
USAGE
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

target_mode="native"
notary_profile="${SIGIL_NOTARY_PROFILE:-Sigil-Notary}"
updater_key="${SIGIL_DESKTOP_UPDATER_KEY:-$HOME/Library/Application Support/sigil/release/desktop-updater.key}"
updater_key_password="${SIGIL_DESKTOP_UPDATER_KEY_PASSWORD:-}"
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
    --updater-key)
      updater_key="${2-}"
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

for command_name in cargo git node pnpm rustup security xcrun codesign ditto shasum tar; do
  command -v "$command_name" >/dev/null || {
    echo "missing required command: $command_name" >&2
    exit 1
  }
done
if [[ ! -f "$updater_key" || -L "$updater_key" ]]; then
  echo "Tauri updater private key must be a regular non-symlink file: $updater_key" >&2
  exit 1
fi
if [[ ! -f "${updater_key}.pub" || -L "${updater_key}.pub" ]]; then
  echo "Tauri updater public key must be a regular non-symlink file: ${updater_key}.pub" >&2
  exit 1
fi
if [[ "$(stat -f '%Lp' "$updater_key")" != "600" ]]; then
  echo "Tauri updater private key must have mode 600: $updater_key" >&2
  exit 1
fi
node -e '
  const fs = require("node:fs");
  const config = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  const embedded = config.plugins?.updater?.pubkey;
  const publisher = fs.readFileSync(process.argv[2], "utf8").trim();
  if (typeof embedded !== "string" || embedded !== publisher) {
    throw new Error("publisher updater public key does not match the embedded Tauri key");
  }
' \
  "$repo_root/apps/desktop/src-tauri/tauri.conf.json" \
  "${updater_key}.pub"

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
mkdir -p "$output_root/.work"

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

  SIGIL_DESKTOP_SIDECAR_TARGET="$target" \
  APPLE_SIGNING_IDENTITY="$identity" \
  TAURI_SIGNING_PRIVATE_KEY="$(<"$updater_key")" \
  TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$updater_key_password" \
    pnpm --dir apps/desktop tauri build \
      --config src-tauri/tauri.bundle.conf.json \
      --config src-tauri/tauri.updater.conf.json \
      --target "$target" \
      --bundles app,dmg

  bundle_root="$repo_root/target/$target/release/bundle"
  source_dmg="$(find_one_dmg "$bundle_root/dmg")"
  source_app="$bundle_root/macos/Sigil.app"
  if [[ ! -d "$source_app" || -L "$source_app" ]]; then
    echo "expected a bundled Sigil.app at $source_app" >&2
    exit 1
  fi
  output_dmg="$output_root/Sigil_${version}_${target}.dmg"
  work_root="$output_root/.work/$target"
  updater_app="$work_root/Sigil.app"
  mkdir -p "$work_root"
  ditto "$source_dmg" "$output_dmg"
  ditto "$source_app" "$updater_app"

  "$repo_root/scripts/verify-desktop-macos.sh" \
    --dmg "$output_dmg" \
    --expected-arch "$expected_arch" \
    --team-id "$team_id" \
    --expected-version "$version" \
    --expected-commit "${commit:0:12}"

  if [[ "$skip_notarization" == false ]]; then
    "$repo_root/scripts/notarize-desktop-macos.sh" \
      --dmg "$output_dmg" \
      --expected-arch "$expected_arch" \
      --team-id "$team_id" \
      --notary-profile "$notary_profile"

    "$repo_root/scripts/notarize-desktop-macos-app.sh" \
      --app "$updater_app" \
      --expected-arch "$expected_arch" \
      --team-id "$team_id" \
      --notary-profile "$notary_profile"
  else
    "$repo_root/scripts/verify-desktop-macos-app.sh" \
      --app "$updater_app" \
      --expected-arch "$expected_arch" \
      --team-id "$team_id" \
      --expected-version "$version" \
      --expected-commit "${commit:0:12}"
  fi

  updater_archive="$output_root/Sigil_${version}_${target}.app.tar.gz"
  tar -czf "$updater_archive" -C "$work_root" Sigil.app
  roundtrip_root="$work_root/archive-roundtrip"
  mkdir -p "$roundtrip_root"
  tar -xzf "$updater_archive" -C "$roundtrip_root"
  roundtrip_verify_args=(
    --app "$roundtrip_root/Sigil.app"
    --expected-arch "$expected_arch"
    --team-id "$team_id"
    --expected-version "$version"
    --expected-commit "${commit:0:12}"
  )
  if [[ "$skip_notarization" == false ]]; then
    roundtrip_verify_args+=(--notarized)
  fi
  "$repo_root/scripts/verify-desktop-macos-app.sh" "${roundtrip_verify_args[@]}"
  TAURI_SIGNING_PRIVATE_KEY_PATH="$updater_key" \
  TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$updater_key_password" \
    pnpm --dir apps/desktop tauri signer sign "$updater_archive"
  test -s "${updater_archive}.sig"
  "$repo_root/scripts/verify-desktop-update-signature.sh" \
    --archive "$updater_archive" \
    --signature "${updater_archive}.sig"

  (
    cd "$output_root"
    shasum -a 256 "$(basename "$output_dmg")" >"$(basename "$output_dmg").sha256"
    shasum -a 256 "$(basename "$updater_archive")" >"$(basename "$updater_archive").sha256"
  )
done

if [[ "${#targets[@]}" -eq 2 ]]; then
  arm_archive="$output_root/Sigil_${version}_aarch64-apple-darwin.app.tar.gz"
  intel_signature="$output_root/Sigil_${version}_x86_64-apple-darwin.app.tar.gz.sig"
  if "$repo_root/scripts/verify-desktop-update-signature.sh" \
    --archive "$arm_archive" \
    --signature "$intel_signature"; then
    echo "swapped Desktop updater signature unexpectedly verified" >&2
    exit 1
  else
    verification_status=$?
    if [[ "$verification_status" -ne 1 ]]; then
      echo "swapped-signature negative control could not complete verification" >&2
      exit "$verification_status"
    fi
  fi
  echo "swapped Desktop updater signature was rejected as expected"
fi

cat >"$output_root/build.txt" <<EOF
version=$version
commit=$commit
team_id=$team_id
identity=$identity
targets=$(printf '%s ' "${targets[@]}")
notarized=$([[ "$skip_notarization" == false ]] && echo true || echo false)
updater_public_key=$(shasum -a 256 "${updater_key}.pub" | awk '{print $1}')
EOF

echo "signed Sigil desktop packages are ready at $output_root"
