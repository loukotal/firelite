#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/release.sh <version> [--yes] [--skip-harness]

Prepares and starts a Firelite release:
  1. verifies the working tree is clean and on main
  2. bumps crates/firelite/Cargo.toml to <version>
  3. refreshes Cargo.lock
  4. runs fmt, clippy, tests, package verification, and SDK harnesses
  5. commits, tags v<version>, pushes main and tag
  6. creates a GitHub Release, which triggers .github/workflows/release.yml

Examples:
  scripts/release.sh 0.3.0
  scripts/release.sh 0.3.0 --yes
  scripts/release.sh 0.3.0 --skip-harness
USAGE
}

confirm() {
  if [[ "${ASSUME_YES}" == "1" ]]; then
    return 0
  fi

  read -r -p "$1 [y/N] " answer
  [[ "${answer}" == "y" || "${answer}" == "Y" ]]
}

run() {
  printf '\n==> %s\n' "$*"
  "$@"
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

VERSION=""
ASSUME_YES=0
SKIP_HARNESS=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    --yes|-y)
      ASSUME_YES=1
      shift
      ;;
    --skip-harness)
      SKIP_HARNESS=1
      shift
      ;;
    -*)
      die "unknown option: $1"
      ;;
    *)
      if [[ -n "${VERSION}" ]]; then
        die "unexpected extra argument: $1"
      fi
      VERSION="$1"
      shift
      ;;
  esac
done

[[ -n "${VERSION}" ]] || {
  usage
  exit 1
}

[[ "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$ ]] || {
  die "version must look like a Cargo semver, for example 0.3.0"
}

TAG="v${VERSION}"
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "${REPO_ROOT}"

CURRENT_BRANCH="$(git branch --show-current)"
[[ "${CURRENT_BRANCH}" == "main" ]] || die "release must run from main, currently on ${CURRENT_BRANCH}"

git diff --quiet || die "working tree has unstaged changes"
git diff --cached --quiet || die "working tree has staged changes"

run git fetch origin --tags
LOCAL_HEAD="$(git rev-parse HEAD)"
REMOTE_HEAD="$(git rev-parse origin/main)"
[[ "${LOCAL_HEAD}" == "${REMOTE_HEAD}" ]] || die "main is not aligned with origin/main"

if git rev-parse -q --verify "refs/tags/${TAG}" >/dev/null; then
  die "local tag ${TAG} already exists"
fi
if git ls-remote --exit-code --tags origin "${TAG}" >/dev/null 2>&1; then
  die "remote tag ${TAG} already exists"
fi

if ! command -v gh >/dev/null 2>&1; then
  die "gh CLI is required to create the GitHub Release"
fi
run gh auth status
if ! gh secret list --repo loukotal/firelite | awk '{print $1}' | grep -qx 'CRATES_TOKEN'; then
  die "GitHub Actions secret CRATES_TOKEN is missing"
fi

CURRENT_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' crates/firelite/Cargo.toml | head -n 1)"
[[ -n "${CURRENT_VERSION}" ]] || die "could not read current crate version"
[[ "${CURRENT_VERSION}" != "${VERSION}" ]] || die "crate is already at version ${VERSION}"

printf '\nRelease plan:\n'
printf '  crate: firelite\n'
printf '  from:  %s\n' "${CURRENT_VERSION}"
printf '  to:    %s\n' "${VERSION}"
printf '  tag:   %s\n' "${TAG}"
printf '\n'

confirm "Continue with release prep and GitHub Release creation?" || die "release cancelled"

perl -0pi -e 's/^version = "\Q'"${CURRENT_VERSION}"'\E"/version = "'"${VERSION}"'"/m' crates/firelite/Cargo.toml

run cargo check -p firelite
run cargo fmt --all -- --check
run cargo clippy --workspace --all-targets -- -D warnings
run cargo test --workspace
run cargo package --package firelite --allow-dirty
if [[ "${SKIP_HARNESS}" != "1" ]]; then
  run node harness/src/test-auth-admin-sdk.mjs
  run node harness/src/test-pubsub-sdk.mjs
fi

run git add Cargo.lock crates/firelite/Cargo.toml
run git commit -m "Release ${VERSION}"
run git tag "${TAG}"
run git push origin main "${TAG}"
run gh release create "${TAG}" --repo loukotal/firelite --title "${TAG}" --notes "Firelite ${VERSION}"

printf '\nRelease created: https://github.com/loukotal/firelite/releases/tag/%s\n' "${TAG}"
printf 'GitHub Actions will publish the crate from .github/workflows/release.yml.\n'
