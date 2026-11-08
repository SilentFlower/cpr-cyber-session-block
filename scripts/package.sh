#!/usr/bin/env bash
# 构建独立插件并由公开 CLI 生成平台安装包
set -euo pipefail

task_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
task_cli=${CPR_PLUGIN_CLI:-cpr-plugin}
task_target=${CPR_PLUGIN_TARGET:-x86_64-unknown-linux-gnu}

pnpm --dir "$task_root/frontend" install --frozen-lockfile
pnpm --dir "$task_root/frontend" lint
pnpm --dir "$task_root/frontend" build
cargo build --manifest-path "$task_root/Cargo.toml" --release --locked --target "$task_target"
python3 "$task_root/scripts/licenses.py" --target "$task_target"
"$task_cli" package \
  --manifest "$task_root/plugin.json" \
  --binary "$task_root/target/$task_target/release/cpr-cyber-session-block" \
  --target "$task_target" \
  --resource-map web=frontend/dist \
  --output-dir "$task_root/dist"
