#!/bin/bash
# Inside the chroot (exocortex-ns.sh exec), as root: installs or updates what
# the phone's Exocortex runs on. Called by scripts/exocortex/install.sh with
# its settings in the environment; idempotent.
#
#   BUN_VERSION / BUN_SHA256   Bun for linux-aarch64-musl, pinned
#   EXOCORTEX_REPO / EXOCORTEX_BRANCH
#   WHALE_HOST_KEY             whale's ssh host key line ("ssh-ed25519 AAAA…")
#   STAGE                      the install stage (system.md, …), in /tmp
set -euo pipefail

say() { printf '  %s\n' "$*"; }

# --- Alpine packages -----------------------------------------------------------
# The rootfs may have been provisioned offline (pinned packages): give apk the
# release's own repositories when it has none.
if ! grep -q '^https\?://' /etc/apk/repositories 2>/dev/null; then
    release="v$(cut -d. -f1,2 /etc/alpine-release)"
    printf 'https://dl-cdn.alpinelinux.org/alpine/%s/main\nhttps://dl-cdn.alpinelinux.org/alpine/%s/community\n' \
        "$release" "$release" >>/etc/apk/repositories
    say "apk: added the $release repositories"
fi
need=()
for pkg in bash curl git openssh-client openssh-keygen ripgrep libstdc++ libgcc ca-certificates unzip procps coreutils; do
    apk info -e "$pkg" >/dev/null 2>&1 || need+=("$pkg")
done
if ((${#need[@]})); then
    say "apk add ${need[*]}"
    apk add --no-cache "${need[@]}" >/dev/null
fi

# --- Bun -------------------------------------------------------------------------
if [[ "$(/opt/bun/bin/bun --version 2>/dev/null)" != "$BUN_VERSION" ]]; then
    say "Bun $BUN_VERSION"
    work="$(mktemp -d)"
    curl -fsSL --retry 3 -o "$work/bun.zip" \
        "https://github.com/oven-sh/bun/releases/download/bun-v$BUN_VERSION/bun-linux-aarch64-musl.zip"
    echo "$BUN_SHA256  $work/bun.zip" | sha256sum -c - >/dev/null
    unzip -q "$work/bun.zip" -d "$work"
    mkdir -p /opt/bun/bin
    install -m 0755 "$work/bun-linux-aarch64-musl/bun" /opt/bun/bin/bun.new
    mv -f /opt/bun/bin/bun.new /opt/bun/bin/bun
    ln -sfn /opt/bun/bin/bun /usr/local/bin/bun
    rm -rf "$work"
fi
say "bun $(/opt/bun/bin/bun --version)"

# --- Claude Code (the agent loop for Claude; not signed in, /proxy lends whale's login) ---
if ! /root/.local/bin/claude --version >/dev/null 2>&1; then
    say "Claude Code (official installer; it verifies the binary against its manifest)"
    curl -fsSL --retry 3 https://claude.ai/install.sh | bash >/dev/null
fi
say "claude $(/root/.local/bin/claude --version 2>/dev/null)"

# --- Exocortex ---------------------------------------------------------------------
if [[ ! -d /opt/exocortex/.git ]]; then
    say "cloning Exocortex ($EXOCORTEX_BRANCH)"
    git clone -q --branch "$EXOCORTEX_BRANCH" "$EXOCORTEX_REPO" /opt/exocortex
else
    cd /opt/exocortex
    if [[ -n "$(git status --porcelain --untracked-files=no)" ]]; then
        say "Exocortex checkout has local changes; not updating it"
    else
        git fetch -q origin "$EXOCORTEX_BRANCH"
        git checkout -q "$EXOCORTEX_BRANCH" 2>/dev/null || git checkout -q -b "$EXOCORTEX_BRANCH" "origin/$EXOCORTEX_BRANCH"
        git merge -q --ff-only "origin/$EXOCORTEX_BRANCH" || say "Exocortex: not a fast-forward; left as it is"
    fi
fi
cd /opt/exocortex
PATH="/opt/bun/bin:$PATH" bun install --frozen-lockfile >/dev/null
say "exocortex $(git rev-parse --short HEAD) ($(git rev-parse --abbrev-ref HEAD))"
mkdir -p /opt/exocortex/config
if [[ ! -e /opt/exocortex/config/system.md ]]; then
    install -m 0644 "$STAGE/system.md" /opt/exocortex/config/system.md
    say "config/system.md installed"
elif ! cmp -s "$STAGE/system.md" /opt/exocortex/config/system.md; then
    install -m 0644 "$STAGE/system.md" /opt/exocortex/config/system.md.oss-phone
    say "config/system.md differs from this install's; the new one is config/system.md.oss-phone"
fi
# Claude through whale (/proxy), set before the daemon's first start.
PATH="/opt/bun/bin:$PATH" bun -e '
  const path = "/opt/exocortex/config/config.json";
  const file = Bun.file(path);
  const config = (await file.exists()) ? await file.json() : {};
  config.providers ??= {};
  config.providers.anthropic ??= {};
  if (!config.providers.anthropic.proxy?.ssh) {
    config.providers.anthropic.proxy = { ssh: "whale" };
    await Bun.write(path, JSON.stringify(config, null, 2) + "\n");
    console.log("  config.json: Claude through /proxy whale");
  }
'

# --- whale: a key of its own, for the restricted /proxy entry ----------------------------
umask 077
mkdir -p /root/.ssh
if [[ ! -f /root/.ssh/exocortex_whale_ed25519 ]]; then
    ssh-keygen -q -t ed25519 -N '' -C "oss-phone-exocortex" -f /root/.ssh/exocortex_whale_ed25519
    say "generated /root/.ssh/exocortex_whale_ed25519 (it never leaves the phone)"
fi
if ! grep -q '^Host whale$' /root/.ssh/config 2>/dev/null; then
    cat >>/root/.ssh/config <<'EOF'

# Exocortex /proxy for Claude (docs/EXOCORTEX.md). whale forces the
# link-only bridge for this key: no shell, no forwarding.
Host whale
    HostName yeyito.dev
    Port 22
    User yeyito
    IdentityFile /root/.ssh/exocortex_whale_ed25519
    IdentitiesOnly yes
EOF
fi
if ! grep -qF "$WHALE_HOST_KEY" /root/.ssh/known_hosts 2>/dev/null; then
    echo "yeyito.dev $WHALE_HOST_KEY" >>/root/.ssh/known_hosts
fi
chmod 0600 /root/.ssh/config /root/.ssh/known_hosts

# --- OSS Phone sources, for the agent to read and redeploy from ---------------------------
umask 022
mkdir -p /opt/src
for repo in OSS-Phone-V1 a26-system; do
    if [[ -d "/opt/src/$repo/.git" ]]; then
        git -C "/opt/src/$repo" pull -q --ff-only 2>/dev/null || say "/opt/src/$repo: not updated (local changes or offline)"
    else
        git clone -q "https://github.com/Yeyito777/$repo.git" "/opt/src/$repo"
    fi
done
say "sources in /opt/src"
