#!/usr/bin/env bash
# 定时跑一轮实弹靶场，归档成绩，并把趋势写回文档。
#
# 它花真钱：每个样本都会真的调用 Provider。默认十二个样本，一轮几十秒到几分钟。
#
# 跑完之后工作区里会多出这些**未提交**的变更，等人来 review 再决定提不提：
#   bench/skill-worker-range/history.jsonl   （多一行）
#   bench/skill-worker-range/runs/*.json     （多一份完整报告）
#   README.md / docs/SKILL_WORKERS.md        （趋势区块被重写）
#
# 故意不自动提交：一轮成绩是不是该进历史，得有人看一眼——尤其是当它变差的时候。
# 这也是为什么它不自动 git pull：测哪版代码由人决定，不由定时器决定。

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

LOG_DIR="$REPO_ROOT/target/skill-worker-range"
mkdir -p "$LOG_DIR"
LOG="$LOG_DIR/weekly.log"

log() {
  echo "[$(date -u '+%Y-%m-%dT%H:%M:%SZ')] $*" | tee -a "$LOG"
}

log "开跑：$(git rev-parse --short HEAD 2>/dev/null || echo '非 git 检出')"

if ! git diff --quiet HEAD 2>/dev/null; then
  # 不是致命错误，但要在日志里留痕：这轮成绩挂在一个没提交的状态上，回放不了。
  log "警告：工作区不干净，这轮成绩无法精确回放。"
fi

if ! ruby scripts/skill_worker_range.rb "$@" >>"$LOG" 2>&1; then
  log "靶场失败，详见 $LOG。趋势不更新——宁可显示上一轮的旧数字，也不显示半轮的假数字。"
  exit 1
fi

ruby scripts/range_trend.rb --inject >>"$LOG" 2>&1
log "完成。待 review 的变更："
git status --short bench README.md docs/SKILL_WORKERS.md | tee -a "$LOG"
