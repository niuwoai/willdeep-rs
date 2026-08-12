import type { Messages } from "./i18n";
import type { RuntimeAgent } from "./RuntimeSidebar";

const LIVE_AGENT_STATUS = ["idle", "working", "running", "queued", "cancelling", "waiting_approval", "waiting_answer", "blocked"];
const RECENTLY_FINISHED_SECONDS = 300;

export function isLiveAgent(agent: RuntimeAgent) {
  return LIVE_AGENT_STATUS.includes(agent.status);
}

/** 侧栏是活动面板，不是归档：只保留在跑的 Agent 和刚结束的，且丢掉没有任何轮次的根 Agent。 */
export function isSidebarAgent(agent: RuntimeAgent) {
  if (isLiveAgent(agent)) return true;
  // Runtime 没记下结束时间的一律当活着，宁可多显示也不隐藏可能还在跑的 Agent。
  if (agent.finished_seconds_ago === null) return true;
  if (!agent.parent_id && agent.current_turn === 0) return false;
  return agent.finished_seconds_ago < RECENTLY_FINISHED_SECONDS;
}

export function formatDuration(seconds: number, t: Messages) {
  if (seconds < 60) return `${seconds}${t.secondsUnit}`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}${t.minutesUnit}${seconds % 60}${t.secondsUnit}`;
  return `${Math.floor(seconds / 3600)}${t.hoursUnit}${Math.floor((seconds % 3600) / 60)}${t.minutesUnit}`;
}

/** 运行中的是「已运行」，结束的是「耗时」——同一个数字混着显示会让人以为进程还活着。 */
export function agentDuration(agent: RuntimeAgent, t: Messages) {
  const label = isLiveAgent(agent) ? t.runningFor : t.duration;
  return `${label} ${formatDuration(agent.elapsed_seconds, t)}`;
}
