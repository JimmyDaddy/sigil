#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  scripts/render-homebrew-formula.sh --version <version> --url <url> --sha256 <sha256> [--formula-name <name>] [--output <path>]
  scripts/render-homebrew-formula.sh --version <version> --arm-url <url> --arm-sha256 <sha256> --intel-url <url> --intel-sha256 <sha256> [--formula-name <name>] [--output <path>]

Render a Homebrew formula for a prebuilt Sigil release archive.
USAGE
}

version=""
formula_name="sigil-ai"
url=""
sha256=""
arm_url=""
arm_sha256=""
intel_url=""
intel_sha256=""
output=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      version="${2-}"
      shift 2
      ;;
    --formula-name)
      formula_name="${2-}"
      shift 2
      ;;
    --url)
      url="${2-}"
      shift 2
      ;;
    --sha256)
      sha256="${2-}"
      shift 2
      ;;
    --arm-url)
      arm_url="${2-}"
      shift 2
      ;;
    --arm-sha256)
      arm_sha256="${2-}"
      shift 2
      ;;
    --intel-url)
      intel_url="${2-}"
      shift 2
      ;;
    --intel-sha256)
      intel_sha256="${2-}"
      shift 2
      ;;
    --output)
      output="${2-}"
      shift 2
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

has_single=0
has_split=0
if [[ -n "${url}" || -n "${sha256}" ]]; then
  has_single=1
fi
if [[ -n "${arm_url}" || -n "${arm_sha256}" || -n "${intel_url}" || -n "${intel_sha256}" ]]; then
  has_split=1
fi

if [[ -z "${version}" || "${has_single}" -eq "${has_split}" ]]; then
  usage >&2
  exit 2
fi
if [[ -z "${formula_name}" || ! "${formula_name}" =~ ^[a-z][a-z0-9_-]*$ ]]; then
  echo "invalid formula name: ${formula_name}" >&2
  exit 2
fi

if [[ "${has_single}" -eq 1 && ( -z "${url}" || -z "${sha256}" ) ]]; then
  usage >&2
  exit 2
fi
if [[ "${has_split}" -eq 1 && ( -z "${arm_url}" || -z "${arm_sha256}" || -z "${intel_url}" || -z "${intel_sha256}" ) ]]; then
  usage >&2
  exit 2
fi

if [[ "${has_single}" -eq 1 ]]; then
  source_block="$(
    cat <<SOURCE
  url "${url}"
  sha256 "${sha256}"
SOURCE
  )"
else
  source_block="$(
    cat <<SOURCE
  on_macos do
    on_arm do
      url "${arm_url}"
      sha256 "${arm_sha256}"
    end

    on_intel do
      url "${intel_url}"
      sha256 "${intel_sha256}"
    end
  end
SOURCE
  )"
fi

formula_class="$(
  printf '%s\n' "${formula_name}" |
    awk -F'[-_]' '{
      for (i = 1; i <= NF; i++) {
        if ($i != "") {
          printf "%s%s", toupper(substr($i, 1, 1)), substr($i, 2)
        }
      }
    }'
)"
if [[ -z "${formula_class}" ]]; then
  echo "unable to derive formula class from ${formula_name}" >&2
  exit 2
fi

formula="$(
  cat <<FORMULA
class ${formula_class} < Formula
  desc "TUI-first Rust AI coding agent"
  homepage "https://github.com/JimmyDaddy/sigil"
  version "${version}"
  license "MIT"

${source_block}

  def install
    bin.install "sigil"
  end

  test do
    assert_match "sigil #{version}", shell_output("#{bin}/sigil --version")
  end
end
FORMULA
)"

if [[ -n "${output}" ]]; then
  mkdir -p "$(dirname "${output}")"
  printf '%s\n' "${formula}" >"${output}"
else
  printf '%s\n' "${formula}"
fi
