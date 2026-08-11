import { useState } from "react";
import { Box, Button, Flex, Input, NativeSelect, Text, Textarea, VStack } from "@chakra-ui/react";
import type { Messages } from "./i18n";
import { RuntimeDetailPanel, type RuntimeDetailTarget } from "./RuntimeDetailPanel";

export type AgentSpawnProfile = "scout" | "reader" | "deep";

export type RuntimeTool = {
  id: string;
  session_id: string | null;
  turn_id: string | null;
  task_id: string;
  agent_id: string;
  name: string;
  status: "running" | "completed" | "failed" | "interrupted";
  started_at_ms: number;
  completed_at_ms: number | null;
};

export type RuntimeArtifact = {
  id: string;
  title: string;
  kind: "workspace_change";
  session_id: string | null;
  turn_id: string | null;
  task_id: string;
  agent_id: string;
  item_count: number;
  created_at: number;
};

export type RuntimeAgent = {
  id: string;
  parent_id: string | null;
  label: string | null;
  background: boolean;
  profile: string | null;
  model: string | null;
  status: string;
  current_turn: number;
  current_tool: string | null;
  total_tokens: number | null;
  elapsed_seconds: number;
  worktree_branch: string | null;
  dedicated_worktree: boolean;
};

export type RuntimeTask = {
  id: string;
  session_id: string | null;
  turn_id: string | null;
  agent_id: string | null;
  status: string;
  profile: string | null;
  elapsed_seconds: number;
  exit_code: number | null;
  failure_domain: string | null;
};

export type RuntimeGate =
  | { kind: "approval"; id: string; task_id: string; description: string; always_allow_available: boolean }
  | { kind: "question"; id: string; task_id: string; question: string; options: string[]; multi_select: boolean };

export type RuntimeActivity = {
  tools: RuntimeTool[];
  artifacts: RuntimeArtifact[];
  agents: RuntimeAgent[];
  tasks: RuntimeTask[];
  gates: RuntimeGate[];
  attention_count: number;
};

type Props = {
  activity: RuntimeActivity;
  messages: Messages;
  onResolveApproval: (id: string, decision: "allow_once" | "deny" | "always_allow") => Promise<void>;
  onAnswerQuestion: (id: string, answer: string | null) => Promise<void>;
  onAgentAction: (id: string, action: "stop" | "retry" | "retry_model" | "prompt", currentModel?: string | null) => Promise<void>;
  canSpawnAgent: boolean;
  onSpawnAgent: (profile: AgentSpawnProfile, prompt: string) => Promise<boolean>;
};

function runtimeStatus(status: string, t: Messages) {
  if (status === "working" || status === "running") return t.working;
  if (status === "queued") return t.queued;
  if (status === "cancelling") return t.cancelling;
  if (status === "waiting_approval") return t.waitingApproval;
  if (status === "waiting_answer") return t.waitingAnswer;
  if (status === "failed") return t.toolFailed;
  if (status === "done" || status === "completed") return t.toolDone;
  if (status === "cancelled") return t.cancelled;
  if (status === "interrupted") return t.interrupted;
  if (status === "blocked") return t.blocked;
  if (status === "idle") return t.idle;
  return t.unknownValue;
}

function failureDomain(domain: string | null, t: Messages) {
  if (domain === "provider") return t.failureProvider;
  if (domain === "policy") return t.failurePolicy;
  if (domain === "tool") return t.failureTool;
  if (domain === "harness") return t.failureHarness;
  if (domain === "internal") return t.failureInternal;
  return t.unknownValue;
}

export function RuntimeSidebar({ activity, messages: t, onResolveApproval, onAnswerQuestion, onAgentAction, canSpawnAgent, onSpawnAgent }: Props) {
  const [answers, setAnswers] = useState<Record<string, string>>({});
  const [multiAnswers, setMultiAnswers] = useState<Record<string, string[]>>({});
  const [controlBusy, setControlBusy] = useState(false);
  const [spawnProfile, setSpawnProfile] = useState<AgentSpawnProfile>("scout");
  const [spawnPrompt, setSpawnPrompt] = useState("");
  const [detail, setDetail] = useState<RuntimeDetailTarget | null>(null);
  const [expanded, setExpanded] = useState(false);
  const runningTools = activity.tools.filter((tool) => tool.status === "running").length;
  async function runControl(action: () => Promise<void>) {
    if (controlBusy) return;
    setControlBusy(true);
    try { await action(); } finally { setControlBusy(false); }
  }
  async function spawnAgent() {
    const task = spawnPrompt.trim();
    if (!canSpawnAgent || !task || controlBusy) return;
    setControlBusy(true);
    try { if (await onSpawnAgent(spawnProfile, task)) setSpawnPrompt(""); } finally { setControlBusy(false); }
  }
  return <Box mt="4" p="3" border="1px solid" borderColor="#202a35" borderRadius="md" bg="#101820">
    <Flex justify="space-between" align="center">
      <Text fontSize="xs" color="#a7b4c4">{t.runtimeActivity}</Text>
      <Button size="2xs" variant="ghost" color="#91a0b2" aria-expanded={expanded} aria-label={expanded ? t.collapse : t.expand} onClick={() => setExpanded((current) => !current)}>{expanded ? t.collapse : t.expand}</Button>
    </Flex>
    {expanded && <>
    <Flex gap="3" wrap="wrap">
      <Text fontSize="sm">{t.tools}: {activity.tools.length}</Text>
      <Text fontSize="sm">{t.running}: {runningTools}</Text>
      <Text fontSize="sm">{t.artifacts}: {activity.artifacts.length}</Text>
    </Flex>
    <Flex gap="3" mt="2" wrap="wrap">
      <Text fontSize="sm">{t.agents}: {activity.agents.length}</Text>
      <Text fontSize="sm">{t.tasks}: {activity.tasks.length}</Text>
      <Text fontSize="sm">{t.needsAttention}: {activity.attention_count}</Text>
    </Flex>
    <Box mt="3" p="2" borderRadius="sm" bg="#171d24">
      <Text fontSize="xs" color="#8290a3" mb="1">{t.newReadOnlyAgent}</Text>
      <NativeSelect.Root mb="1">
        <NativeSelect.Field aria-label={t.agentProfile} value={spawnProfile} onChange={(event) => setSpawnProfile(event.target.value as AgentSpawnProfile)} fontSize="xs" h="8">
          <option value="scout">{t.agentProfileScout}</option>
          <option value="reader">{t.agentProfileReader}</option>
          <option value="deep">{t.agentProfileDeep}</option>
        </NativeSelect.Field>
        <NativeSelect.Indicator />
      </NativeSelect.Root>
      <Textarea size="xs" rows={2} resize="none" value={spawnPrompt} aria-label={t.agentTask} placeholder={t.agentTaskPlaceholder} onChange={(event) => setSpawnPrompt(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void spawnAgent(); } }} />
      <Flex mt="1" justify="flex-end">
        <Button size="2xs" disabled={!canSpawnAgent || !spawnPrompt.trim() || controlBusy} onClick={() => void spawnAgent()}>{t.spawnAgent}</Button>
      </Flex>
      {!canSpawnAgent && <Text mt="1" fontSize="2xs" color="#718096">{t.activeSessionRequired}</Text>}
    </Box>
    {activity.gates.length > 0 && <VStack align="stretch" gap="2" mt="3">
      {activity.gates.slice(0, 3).map((gate) => <Box key={gate.id} p="2" borderRadius="sm" bg="#171d24">
        <Flex justify="space-between" gap="2"><Text fontSize="xs" color="#e5b96f">{gate.kind === "approval" ? t.waitingApproval : t.waitingAnswer}</Text><Button size="2xs" variant="ghost" onClick={() => setDetail({ kind: "task", id: gate.task_id })}>{t.details}</Button></Flex>
        <Text fontSize="xs" lineClamp="2">{gate.kind === "approval" ? gate.description : gate.question}</Text>
        {gate.kind === "approval" ? <Flex gap="1" mt="2" wrap="wrap">
          <Button size="2xs" disabled={controlBusy} onClick={() => void runControl(() => onResolveApproval(gate.id, "allow_once"))}>{t.allowOnce}</Button>
          <Button size="2xs" disabled={controlBusy} variant="outline" onClick={() => void runControl(() => onResolveApproval(gate.id, "deny"))}>{t.deny}</Button>
          {gate.always_allow_available && <Button size="2xs" disabled={controlBusy} variant="ghost" onClick={() => void runControl(() => onResolveApproval(gate.id, "always_allow"))}>{t.alwaysAllow}</Button>}
        </Flex> : <Box mt="2">
          <Flex gap="1" wrap="wrap">
            {gate.options.map((option) => gate.multi_select
              ? <label key={option}><input type="checkbox" checked={(multiAnswers[gate.id] ?? []).includes(option)} onChange={(event) => setMultiAnswers((current) => ({ ...current, [gate.id]: event.target.checked ? [...(current[gate.id] ?? []), option] : (current[gate.id] ?? []).filter((value) => value !== option) }))} /> <Text as="span" fontSize="xs">{option}</Text></label>
              : <Button key={option} size="2xs" disabled={controlBusy} onClick={() => void runControl(() => onAnswerQuestion(gate.id, option))}>{option}</Button>)}
          </Flex>
          {gate.multi_select && <Button mt="1" size="2xs" disabled={controlBusy || !(multiAnswers[gate.id]?.length)} onClick={() => void runControl(() => onAnswerQuestion(gate.id, (multiAnswers[gate.id] ?? []).join(", ")))}>{t.submitAnswer}</Button>}
          <Flex mt="1" gap="1"><Input size="xs" value={answers[gate.id] ?? ""} placeholder={t.otherAnswer} onChange={(event) => setAnswers((current) => ({ ...current, [gate.id]: event.target.value }))} /><Button size="2xs" disabled={controlBusy || !(answers[gate.id]?.trim())} onClick={() => void runControl(() => onAnswerQuestion(gate.id, answers[gate.id].trim()))}>{t.submitAnswer}</Button></Flex>
        </Box>}
      </Box>)}
    </VStack>}
    {activity.tasks.length > 0 && <VStack align="stretch" gap="1" mt="3">
      <Text fontSize="2xs" color="#8290a3">{t.tasks}</Text>
      {activity.tasks.slice(0, 4).map((task) => <Box key={task.id} p="1" borderRadius="sm" bg="#131a22">
        <Flex justify="space-between" gap="2" align="center">
          <Text fontSize="xs" overflow="hidden" textOverflow="ellipsis" whiteSpace="nowrap">{task.profile || t.task}</Text>
          <Flex align="center" gap="1" flexShrink="0">
            <Text fontSize="2xs" color="#8290a3">{runtimeStatus(task.status, t)} · {task.elapsed_seconds}{t.secondsUnit}</Text>
            <Button size="2xs" variant="ghost" onClick={() => setDetail({ kind: "task", id: task.id })}>{t.details}</Button>
          </Flex>
        </Flex>
        {(task.exit_code !== null || task.failure_domain) && <Text fontSize="2xs" color="#718096">
          {task.exit_code !== null ? `${t.exitCode}: ${task.exit_code}` : ""}{task.exit_code !== null && task.failure_domain ? " · " : ""}{task.failure_domain ? `${t.failureDomain}: ${failureDomain(task.failure_domain, t)}` : ""}
        </Text>}
      </Box>)}
    </VStack>}
    {activity.agents.length > 0 && <VStack align="stretch" gap="1" mt="3">
      {activity.agents.slice(0, 4).map((agent) => <Box key={agent.id} pl={agent.parent_id ? "3" : "0"}>
        <Flex justify="space-between" gap="2" fontSize="xs"><Text overflow="hidden" textOverflow="ellipsis" whiteSpace="nowrap">{agent.label || agent.profile || t.agent}</Text><Flex align="center" gap="1" flexShrink="0"><Text color="#8290a3">{runtimeStatus(agent.status, t)} · {t.turn} {agent.current_turn}</Text><Button size="2xs" variant="ghost" onClick={() => setDetail({ kind: "agent", id: agent.id })}>{t.details}</Button></Flex></Flex>
        <Text fontSize="2xs" color="#718096" overflow="hidden" textOverflow="ellipsis" whiteSpace="nowrap">{agent.model || t.unknownValue} · {agent.total_tokens ?? t.unknownValue} {t.tokenUnit} · {agent.elapsed_seconds}{t.secondsUnit}{agent.dedicated_worktree ? ` · ${agent.worktree_branch || t.worktree}` : ""}</Text>
        {agent.background && <Flex gap="1" mt="1">
          {agent.status === "working" && <><Button size="2xs" variant="ghost" disabled={controlBusy} onClick={() => void runControl(() => onAgentAction(agent.id, "prompt"))}>{t.instruct}</Button><Button size="2xs" variant="ghost" disabled={controlBusy} onClick={() => void runControl(() => onAgentAction(agent.id, "stop"))}>{t.stop}</Button></>}
          {["blocked", "failed", "done", "cancelled"].includes(agent.status) && <><Button size="2xs" variant="ghost" disabled={controlBusy} onClick={() => void runControl(() => onAgentAction(agent.id, "retry"))}>{t.retry}</Button><Button size="2xs" variant="ghost" disabled={controlBusy} onClick={() => void runControl(() => onAgentAction(agent.id, "retry_model", agent.model))}>{t.changeModel}</Button></>}
        </Flex>}
      </Box>)}
    </VStack>}
    {detail && <RuntimeDetailPanel activity={activity} messages={t} target={detail} onClose={() => setDetail(null)} />}
    {activity.tools[0] && <Text mt="2" fontSize="xs" color="#718096" overflow="hidden" textOverflow="ellipsis" whiteSpace="nowrap">
      {activity.tools[0].name} · {activity.tools[0].status === "running" ? t.toolRunning : activity.tools[0].status === "completed" ? t.toolDone : t.toolFailed}
    </Text>}
    </>}
  </Box>;
}
