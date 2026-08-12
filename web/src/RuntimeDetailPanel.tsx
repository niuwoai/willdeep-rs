import { Box, Button, Flex, Text, VStack } from "@chakra-ui/react";
import type { Messages } from "./i18n";
import type { RuntimeActivity, RuntimeAgent, RuntimeArtifact, RuntimeGate, RuntimeTask, RuntimeTool } from "./RuntimeSidebar";
import { agentDuration, formatDuration } from "./runtimeAgents";

export type RuntimeDetailTarget = { kind: "agent" | "task"; id: string };

type Props = {
  activity: RuntimeActivity;
  messages: Messages;
  target: RuntimeDetailTarget;
  onClose: () => void;
};

function statusLabel(status: string, t: Messages) {
  if (status === "working" || status === "running") return t.working;
  if (status === "queued") return t.queued;
  if (status === "cancelling") return t.cancelling;
  if (status === "waiting_approval") return t.waitingApproval;
  if (status === "waiting_answer") return t.waitingAnswer;
  if (status === "completed" || status === "done") return t.toolDone;
  if (status === "failed") return t.toolFailed;
  if (status === "interrupted") return t.interrupted;
  if (status === "cancelled") return t.cancelled;
  if (status === "blocked") return t.blocked;
  if (status === "idle") return t.idle;
  return t.unknownValue;
}

function failureDomainLabel(domain: string | null, t: Messages) {
  if (domain === "provider") return t.failureProvider;
  if (domain === "policy") return t.failurePolicy;
  if (domain === "tool") return t.failureTool;
  if (domain === "harness") return t.failureHarness;
  if (domain === "internal") return t.failureInternal;
  return t.unknownValue;
}

function toolDuration(tool: RuntimeTool) {
  const end = tool.completed_at_ms ?? Date.now();
  return (Math.max(0, end - tool.started_at_ms) / 1000).toFixed(1);
}

function agentTools(activity: RuntimeActivity, agent: RuntimeAgent) {
  return activity.tools.filter((tool) => tool.agent_id === agent.id);
}

function agentArtifacts(activity: RuntimeActivity, agent: RuntimeAgent) {
  return activity.artifacts.filter((artifact) => artifact.agent_id === agent.id);
}

function taskGate(activity: RuntimeActivity, id: string): RuntimeGate | undefined {
  return activity.gates.find((gate) => gate.task_id === id);
}

function runtimeTask(activity: RuntimeActivity, id: string): RuntimeTask | undefined {
  return activity.tasks.find((task) => task.id === id);
}

function taskTools(activity: RuntimeActivity, id: string) {
  return activity.tools.filter((tool) => tool.task_id === id);
}

function taskArtifacts(activity: RuntimeActivity, id: string) {
  return activity.artifacts.filter((artifact) => artifact.task_id === id);
}

function StructuredTimeline({ tools, artifacts, messages: t }: { tools: RuntimeTool[]; artifacts: RuntimeArtifact[]; messages: Messages }) {
  const sortedTools = [...tools].sort((left, right) => right.started_at_ms - left.started_at_ms).slice(0, 12);
  const sortedArtifacts = [...artifacts].sort((left, right) => right.created_at - left.created_at).slice(0, 8);
  if (!sortedTools.length && !sortedArtifacts.length) return <Text fontSize="xs" color="#718096">{t.noStructuredActivity}</Text>;
  return <VStack align="stretch" gap="1">
    {sortedTools.length > 0 && <Text fontSize="2xs" color="#8290a3">{t.structuredToolLog}</Text>}
    {sortedTools.map((tool) => <Flex key={tool.id} justify="space-between" gap="2" fontSize="xs">
      <Text overflow="hidden" textOverflow="ellipsis" whiteSpace="nowrap">{tool.name}</Text>
      <Text color="#8290a3" flexShrink="0">{statusLabel(tool.status, t)} · {toolDuration(tool)}{t.secondsUnit}</Text>
    </Flex>)}
    {sortedArtifacts.length > 0 && <Text mt="1" fontSize="2xs" color="#8290a3">{t.diffArtifacts}</Text>}
    {sortedArtifacts.map((artifact) => <Flex key={artifact.id} justify="space-between" gap="2" fontSize="xs">
      <Text overflow="hidden" textOverflow="ellipsis" whiteSpace="nowrap">{artifact.title}</Text>
      <Text color="#8290a3" flexShrink="0">{artifact.item_count} {t.changedItems}</Text>
    </Flex>)}
  </VStack>;
}

export function RuntimeDetailPanel({ activity, messages: t, target, onClose }: Props) {
  const agent = target.kind === "agent" ? activity.agents.find((candidate) => candidate.id === target.id) : undefined;
  const task = target.kind === "task" ? runtimeTask(activity, target.id) : undefined;
  const gate = target.kind === "task" ? taskGate(activity, target.id) : undefined;
  if (target.kind === "agent" && !agent) return null;
  if (target.kind === "task" && !task) return null;
  const tools = agent ? agentTools(activity, agent) : taskTools(activity, target.id);
  const artifacts = agent ? agentArtifacts(activity, agent) : taskArtifacts(activity, target.id);
  return <Box mt="3" p="2" border="1px solid" borderColor="#2b3948" borderRadius="sm" bg="#0b1118">
    <Flex justify="space-between" align="center" gap="2" mb="2">
      <Text fontSize="xs" fontWeight="semibold">{agent ? t.agentDetails : t.taskDetails}</Text>
      <Button size="2xs" variant="ghost" aria-label={t.closeDetails} title={t.closeDetails} onClick={onClose}>×</Button>
    </Flex>
    {agent && <VStack align="stretch" gap="1" mb="2">
      <Flex justify="space-between" gap="2" fontSize="xs"><Text>{agent.label || agent.profile || t.agent}</Text><Text color="#8290a3">{statusLabel(agent.status, t)}</Text></Flex>
      <Text fontSize="2xs" color="#718096">{agent.profile || t.unknownValue} · {agent.model || t.unknownValue}</Text>
      <Text fontSize="2xs" color="#718096">{t.turn} {agent.current_turn} · {agent.total_tokens ?? t.unknownValue} {t.tokenUnit} · {agentDuration(agent, t)}</Text>
      {agent.current_tool && <Text fontSize="2xs" color="#718096">{t.currentTool}: {agent.current_tool}</Text>}
      {agent.dedicated_worktree && <Text fontSize="2xs" color="#718096">{t.worktree}: {agent.worktree_branch || t.unknownValue}</Text>}
    </VStack>}
    {task && <VStack align="stretch" gap="1" mb="2">
      <Flex justify="space-between" gap="2" fontSize="xs"><Text>{task.profile || t.task}</Text><Text color="#8290a3">{statusLabel(task.status, t)}</Text></Flex>
      <Text fontSize="2xs" color="#718096">{t.duration}: {formatDuration(task.elapsed_seconds, t)}</Text>
      {task.exit_code !== null && <Text fontSize="2xs" color="#718096">{t.exitCode}: {task.exit_code}</Text>}
      {task.failure_domain && <Text fontSize="2xs" color="#718096">{t.failureDomain}: {failureDomainLabel(task.failure_domain, t)}</Text>}
    </VStack>}
    {gate && <Text fontSize="xs" mb="2">{gate.kind === "approval" ? t.waitingApproval : t.waitingAnswer}</Text>}
    <StructuredTimeline tools={tools} artifacts={artifacts} messages={t} />
    <Text mt="2" fontSize="2xs" color="#596575">{t.structuredLogPrivacy}</Text>
  </Box>;
}
