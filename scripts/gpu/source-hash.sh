# Sourced by scripts/gpu/build.sh and install.sh: a hash of the compositor's
# sources (src/moon-gpu), stamped next to the binaries a build produces, so
# an install can tell an image built from other sources (another worktree,
# an older checkout) from a current one.
moon_gpu_source_hash() {
    local sum=sha256sum
    command -v sha256sum >/dev/null || sum="shasum -a 256"
    (cd "$PROJECT_ROOT/src/moon-gpu" &&
        find . -type f ! -name '.*' | LC_ALL=C sort | while read -r f; do
            $sum "$f"
        done) | $sum | cut -c1-16
}
