#!/usr/bin/env bash
# Optional integration suite against a sibling Host checkout; uses in-memory SQLite only.
set -euo pipefail
cd "$(dirname "$0")/.."
host_repo="$(realpath "${1:-../BroKnowMyQQBot}")"
test_root="$(mktemp -d /tmp/bkmqb-issue-chat.XXXXXX)"
trap 'rm -rf "$test_root"' EXIT
python3 - "$PWD" "$host_repo" "$test_root" <<'PY'
import json
from pathlib import Path
import sys
repo, host, target = map(Path, sys.argv[1:])
(target / 'src').mkdir()
for name in ['aliases.rs', 'management.rs', 'management_tests.rs']:
    (target / 'src' / name).symlink_to(repo / 'src' / name)
(target / 'wit').symlink_to(repo / 'wit')
manifest = (repo / 'Cargo.toml').read_text() + '\n[dev-dependencies]\n'
for name in ['plugin-host', 'plugin-api']:
    manifest += f'{name} = {{ path = {json.dumps(str(host / "crates" / name))} }}\n'
(target / 'Cargo.toml').write_text(manifest)
source = (repo / 'src/lib.rs').read_text()
source += '\n#[cfg(test)]\n#[path = ' + json.dumps(str(repo / 'scripts/host_chat_tests.rs')) + ']\nmod host_chat_tests;\n'
(target / 'src/lib.rs').write_text(source)
PY
CARGO_TARGET_DIR="${BKM_ISSUE_CHAT_TARGET_DIR:-/tmp/bkmqb-issue-chat-target}" \
  cargo test --manifest-path "$test_root/Cargo.toml" host_chat_tests -- --nocapture
