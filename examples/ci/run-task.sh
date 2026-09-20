#!/usr/bin/env bash
# 在 CI 里跑一轮 willdeep：结果与审计报告落成产物，退出码原样交给作业。
#
#   examples/ci/run-task.sh <prompt-file> [workspace]
#
# 环境变量：
#   SOMEIM_API_KEY / ANTHROPIC_API_KEY   凭据（零配置，见 docs/AUTHENTICATION.md）
#   WILLDEEP_MODEL                        可选，覆盖缺省模型
#   WILLDEEP_MAX_TURNS                    可选，模型调用上限，缺省 40
#   WILLDEEP_ARTIFACTS                    可选，产物目录，缺省 willdeep-artifacts
#   WILLDEEP_HOME                         可选，状态目录，缺省 $PWD/.willdeep-ci（隔离本次作业）
#
# 说明见 docs/CI_INTEGRATION.md。

set -euo pipefail

PROMPT_FILE="${1:?usage: run-task.sh <prompt-file> [workspace]}"
WORKSPACE="${2:-$PWD}"
ARTIFACTS="${WILLDEEP_ARTIFACTS:-willdeep-artifacts}"
MAX_TURNS="${WILLDEEP_MAX_TURNS:-40}"
export WILLDEEP_HOME="${WILLDEEP_HOME:-$PWD/.willdeep-ci}"

if ! command -v willdeep >/dev/null 2>&1; then
  echo "willdeep is not installed; see docs/CI_INTEGRATION.md" >&2
  exit 2
fi
if [ ! -s "$PROMPT_FILE" ]; then
  echo "prompt file is missing or empty: $PROMPT_FILE" >&2
  exit 2
fi

mkdir -p "$ARTIFACTS" "$WILLDEEP_HOME"
willdeep --version | tee "$ARTIFACTS/version.txt"

model_args=()
if [ -n "${WILLDEEP_MODEL:-}" ]; then
  model_args=(--model "$WILLDEEP_MODEL")
fi

# 提示词从文件来，不拼进命令行；stdout 只有一个 JSON 对象，原样落盘。
set +e
willdeep --workspace "$WORKSPACE" --full-auto --max-turns "$MAX_TURNS" "${model_args[@]}" \
  run --local --output json --input "$PROMPT_FILE" > "$ARTIFACTS/result.json"
code=$?
set -e

echo "willdeep run exit code: $code"
if [ -s "$ARTIFACTS/result.json" ]; then
  cat "$ARTIFACTS/result.json"
  echo
fi

# WILLDEEP_HOME 是本次作业独占的，所以 latest 就是刚才那个会话。
# 审计导出失败不改变作业结果：没跑起来的作业本来就没有会话可审。
willdeep audit export --session latest --json --output "$ARTIFACTS/audit.json" \
  || echo "audit export skipped: no session recorded" >&2
willdeep audit export --session latest --output "$ARTIFACTS/audit.md" >/dev/null 2>&1 || true

case "$code" in
  0) echo "completed" ;;
  5) echo "partial result: turn limit, tool failure or verification not passed; see result.json" ;;
  4) echo "blocked: a command needed human approval and nobody can answer in CI" ;;
  3) echo "provider error: credentials, quota or network" ;;
  2) echo "input error: prompt file or arguments" ;;
  *) echo "configuration or internal error" ;;
esac
exit "$code"
