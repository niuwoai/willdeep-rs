#!/usr/bin/env bash
# 每周给线上派工指标拍一张快照，归档，把趋势写回文档，没达标就报警。
#
# 它不花钱、不联网：只跑 `willdeep daemon agent-metrics --json`，读本机 Runtime 的
# agent 记录，出来的全是计数和比率。Runtime 没起时 CLI 会顺手把它拉起来。
#
# 跑完之后工作区里会多出这些**未提交**的变更，等人来 review 再决定提不提：
#   bench/agent-metrics/history.jsonl        （多一行）
#   README.md / docs/AGENT_METRICS.md        （趋势区块被重写）
#
# 故意不自动提交、不自动 git pull：一张快照进不进公开历史得有人看一眼——尤其是
# 当它不好看的时候。退出码：0 正常；1 快照没拍成（趋势不更新）；2 拍成了但有指标没达标。

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

LOG_DIR="$REPO_ROOT/target/agent-metrics"
mkdir -p "$LOG_DIR"
LOG="$LOG_DIR/weekly.log"

log() {
  echo "[$(date -u '+%Y-%m-%dT%H:%M:%SZ')] $*" | tee -a "$LOG"
}

log "开拍：$(git rev-parse --short HEAD 2>/dev/null || echo '非 git 检出') · 窗口 ${WILLDEEP_METRICS_WINDOW:-7d}"

if ! git diff --quiet HEAD 2>/dev/null; then
  log "警告：工作区不干净，这张快照挂在一个没提交的状态上。"
fi

if ! ruby scripts/agent_metrics_publish.rb --window "${WILLDEEP_METRICS_WINDOW:-7d}" "$@" >>"$LOG" 2>&1; then
  log "快照失败，详见 $LOG。趋势不更新——宁可显示上一张的旧数字，也不显示半张的假数字。"
  exit 1
fi

if ruby scripts/agent_metrics_trend.rb --inject --alarm >>"$LOG" 2>&1; then
  log "完成。待 review 的变更："
  git status --short bench/agent-metrics README.md docs/AGENT_METRICS.md | tee -a "$LOG"
else
  log "⚠️ 报警：窗口内有指标没达到设计目标，详见 $LOG 与 docs/AGENT_METRICS.md。"
  git status --short bench/agent-metrics README.md docs/AGENT_METRICS.md | tee -a "$LOG"
  exit 2
fi
