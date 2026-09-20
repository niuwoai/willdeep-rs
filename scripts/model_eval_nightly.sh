#!/usr/bin/env bash
# 每晚跑一轮固定任务集，归档成绩，把趋势写回 docs/MODEL_EVAL.md，掉点就报警。
#
# 它花真钱：二十个任务 × 每个模型各跑一次 `willdeep run`。模型列表来自
# WILLDEEP_EVAL_MODELS（逗号分隔），缺省 glm-5,deepseek-v4-flash,deepseek-v4-pro。
#
# 跑完之后工作区里会多出这些**未提交**的变更，等人来 review 再决定提不提：
#   bench/model-eval/history.jsonl          （每个模型多一行）
#   bench/model-eval/reports/<日期>/*       （每个模型一份 JSON + Markdown）
#   docs/MODEL_EVAL.md                       （趋势区块被重写）
#
# 故意不自动提交、不自动 git pull：测哪版代码由人决定，成绩进不进历史也由人决定，
# 尤其是当它变差的时候。退出码：0 正常；1 评测没跑成（趋势不更新）；2 跑成了但报警。

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

LOG_DIR="$REPO_ROOT/target/model-eval"
mkdir -p "$LOG_DIR"
LOG="$LOG_DIR/nightly.log"

log() {
  echo "[$(date -u '+%Y-%m-%dT%H:%M:%SZ')] $*" | tee -a "$LOG"
}

MODELS="${WILLDEEP_EVAL_MODELS:-glm-5,deepseek-v4-flash,deepseek-v4-pro}"
args=()
for model in ${MODELS//,/ }; do
  args+=(--model "$model")
done

log "开跑：$(git rev-parse --short HEAD 2>/dev/null || echo '非 git 检出') · 模型 $MODELS"

if ! git diff --quiet HEAD 2>/dev/null; then
  log "警告：工作区不干净，这轮成绩无法精确回放。"
fi

if ! ruby scripts/model_eval.rb "${args[@]}" "$@" >>"$LOG" 2>&1; then
  log "评测失败，详见 $LOG。趋势不更新——宁可显示上一轮的旧数字，也不显示半轮的假数字。"
  exit 1
fi

if ruby scripts/model_eval_trend.rb --inject --alarm >>"$LOG" 2>&1; then
  log "完成。待 review 的变更："
  git status --short bench/model-eval docs/MODEL_EVAL.md | tee -a "$LOG"
else
  log "⚠️ 报警：通过率或人话率比基线掉了 10 个点以上，详见 $LOG 与 docs/MODEL_EVAL.md。"
  git status --short bench/model-eval docs/MODEL_EVAL.md | tee -a "$LOG"
  exit 2
fi
