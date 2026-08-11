import { useState } from "react";
import { Box, Button, Flex, Input, Text, VStack } from "@chakra-ui/react";
import type { Messages } from "./i18n";

export type RuntimeTool = {
  id: string;
  name: string;
  status: "running" | "completed" | "failed" | "interrupted";
  started_at_ms: number;
  completed_at_ms: number | null;
};

export type RuntimeArtifact = {
  id: string;
  title: string;
  kind: "workspace_change";
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

export type RuntimeGate =
  | { kind: "approval"; id: string; task_id: string; description: string; always_allow_available: boolean }
  | { kind: "question"; id: string; task_id: string; question: string; options: string[]; multi_select: boolean };

export type RuntimeActivity = {
  tools: RuntimeTool[];
  artifacts: RuntimeArtifact[];
  agents: RuntimeAgent[];
  gates: RuntimeGate[];
  attention_count: number;
};

type Props = {
  activity: RuntimeActivity;
  messages: Messages;
  busy: boolean;
  onResolveApproval: (id: string, decision: "allow_once" | "deny" | "always_allow") => Promise<void>;
  onAnswerQuestion: (id: string, answer: string | null) => Promise<void>;
  onAgentAction: (id: string, action: "stop" | "retry" | "prompt") => Promise<void>;
};

function agentStatus(status: string, t: Messages) {
  if (status === "working") return t.working;
  if (status === "waiting_approval") return t.waitingApproval;
  if (status === "waiting_answer") return t.waitingAnswer;
  if (status === "failed") return t.toolFailed;
  if (status === "done") return t.toolDone;
  if (status === "blocked") return t.blocked;
  return status;
}

export function RuntimeSidebar({ activity, messages: t, busy, onResolveApproval, onAnswerQuestion, onAgentAction }: Props) {
  const [answers, setAnswers] = useState<Record<string, string>>({});
  const [multiAnswers, setMultiAnswers] = useState<Record<string, string[]>>({});
  const runningTools = activity.tools.filter((tool) => tool.status === "running").length;
  return <Box mt="4" p="3" border="1px solid" borderColor="#202a35" borderRadius="md" bg="#101820">
    <Text fontSize="xs" color="#8290a3" mb="2">{t.runtimeActivity}</Text>
    <Flex gap="3" wrap="wrap">
      <Text fontSize="sm">{t.tools}: {activity.tools.length}</Text>
      <Text fontSize="sm">{t.running}: {runningTools}</Text>
      <Text fontSize="sm">{t.artifacts}: {activity.artifacts.length}</Text>
    </Flex>
    <Flex gap="3" mt="2" wrap="wrap">
      <Text fontSize="sm">{t.agents}: {activity.agents.length}</Text>
      <Text fontSize="sm">{t.needsAttention}: {activity.attention_count}</Text>
    </Flex>
    {activity.gates.length > 0 && <VStack align="stretch" gap="2" mt="3">
      {activity.gates.slice(0, 3).map((gate) => <Box key={gate.id} p="2" borderRadius="sm" bg="#171d24">
        <Text fontSize="xs" color="#e5b96f">{gate.kind === "approval" ? t.waitingApproval : t.waitingAnswer}</Text>
        <Text fontSize="xs" lineClamp="2">{gate.kind === "approval" ? gate.description : gate.question}</Text>
        {gate.kind === "approval" ? <Flex gap="1" mt="2" wrap="wrap">
          <Button size="2xs" disabled={busy} onClick={() => void onResolveApproval(gate.id, "allow_once")}>{t.allowOnce}</Button>
          <Button size="2xs" disabled={busy} variant="outline" onClick={() => void onResolveApproval(gate.id, "deny")}>{t.deny}</Button>
          {gate.always_allow_available && <Button size="2xs" disabled={busy} variant="ghost" onClick={() => void onResolveApproval(gate.id, "always_allow")}>{t.alwaysAllow}</Button>}
        </Flex> : <Box mt="2">
          <Flex gap="1" wrap="wrap">
            {gate.options.map((option) => gate.multi_select
              ? <label key={option}><input type="checkbox" checked={(multiAnswers[gate.id] ?? []).includes(option)} onChange={(event) => setMultiAnswers((current) => ({ ...current, [gate.id]: event.target.checked ? [...(current[gate.id] ?? []), option] : (current[gate.id] ?? []).filter((value) => value !== option) }))} /> <Text as="span" fontSize="xs">{option}</Text></label>
              : <Button key={option} size="2xs" disabled={busy} onClick={() => void onAnswerQuestion(gate.id, option)}>{option}</Button>)}
          </Flex>
          {gate.multi_select && <Button mt="1" size="2xs" disabled={busy || !(multiAnswers[gate.id]?.length)} onClick={() => void onAnswerQuestion(gate.id, (multiAnswers[gate.id] ?? []).join(", "))}>{t.submitAnswer}</Button>}
          <Flex mt="1" gap="1"><Input size="xs" value={answers[gate.id] ?? ""} placeholder={t.otherAnswer} onChange={(event) => setAnswers((current) => ({ ...current, [gate.id]: event.target.value }))} /><Button size="2xs" disabled={busy || !(answers[gate.id]?.trim())} onClick={() => void onAnswerQuestion(gate.id, answers[gate.id].trim())}>{t.submitAnswer}</Button></Flex>
        </Box>}
      </Box>)}
    </VStack>}
    {activity.agents.length > 0 && <VStack align="stretch" gap="1" mt="3">
      {activity.agents.slice(0, 4).map((agent) => <Box key={agent.id} pl={agent.parent_id ? "3" : "0"}>
        <Flex justify="space-between" gap="2" fontSize="xs"><Text overflow="hidden" textOverflow="ellipsis" whiteSpace="nowrap">{agent.label || agent.profile || t.agent}</Text><Text color="#8290a3" flexShrink="0">{agentStatus(agent.status, t)} · {t.turn} {agent.current_turn}</Text></Flex>
        <Text fontSize="2xs" color="#718096" overflow="hidden" textOverflow="ellipsis" whiteSpace="nowrap">{agent.model || t.unknownValue} · {agent.total_tokens ?? t.unknownValue} {t.tokenUnit} · {agent.elapsed_seconds}{t.secondsUnit}{agent.dedicated_worktree ? ` · ${agent.worktree_branch || t.worktree}` : ""}</Text>
        {agent.background && <Flex gap="1" mt="1">
          {agent.status === "working" && <><Button size="2xs" variant="ghost" disabled={busy} onClick={() => void onAgentAction(agent.id, "prompt")}>{t.instruct}</Button><Button size="2xs" variant="ghost" disabled={busy} onClick={() => void onAgentAction(agent.id, "stop")}>{t.stop}</Button></>}
          {["blocked", "failed", "done", "cancelled"].includes(agent.status) && <Button size="2xs" variant="ghost" disabled={busy} onClick={() => void onAgentAction(agent.id, "retry")}>{t.retry}</Button>}
        </Flex>}
      </Box>)}
    </VStack>}
    {activity.tools[0] && <Text mt="2" fontSize="xs" color="#718096" overflow="hidden" textOverflow="ellipsis" whiteSpace="nowrap">
      {activity.tools[0].name} · {activity.tools[0].status === "running" ? t.toolRunning : activity.tools[0].status === "completed" ? t.toolDone : t.toolFailed}
    </Text>}
  </Box>;
}
