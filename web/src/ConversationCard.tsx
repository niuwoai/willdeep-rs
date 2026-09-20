import type { Messages } from "./i18n";
import { Markdown } from "./Markdown";
import "./conversation.css";

type StepStatus = "pending" | "in_progress" | "done" | "skipped" | "failed";
export type Plan = { summary: string; steps: Array<{ id: string; text: string; status: StepStatus; detail?: string | null }> };
export type ConversationItem = {
  role: "user" | "assistant" | "system" | "plan";
  content: string;
  attachment_count: number;
  plan?: Plan;
  details?: string[];
  /// 这条助手消息发起的工具调用：名字、脱敏摘要、有无结果。原始参数不下发。
  tools?: { name: string; detail?: string; completed: boolean }[];
};

export function ConversationCard({ plan, details = [], messages: t }: { plan?: Plan; details?: string[]; messages: Messages }) {
  const statuses: Record<StepStatus, string> = {
    pending: t.planPending, in_progress: t.toolRunning, done: t.toolDone,
    skipped: t.planSkipped, failed: t.toolFailed,
  };
  const finished = plan?.steps.filter((step) => step.status === "done" || step.status === "skipped").length ?? 0;
  return <section className="conversation-card" aria-label={plan ? t.planTitle : t.hostActivity}>
    <div className="conversation-card-heading">
      <strong>{plan ? t.planTitle : t.hostActivity}</strong>
      {plan && <span>{t.planProgress.replace("{finished}", String(finished)).replace("{total}", String(plan.steps.length))}</span>}
    </div>
    {plan?.summary && <Markdown content={plan.summary} />}
    {plan && <ol className="conversation-plan-steps">
      {plan.steps.map((step) => <li key={step.id} data-status={step.status}>
        <span className="conversation-plan-status">{statuses[step.status]}</span>
        <div><span>{step.text}</span>{step.detail && <p>{step.detail}</p>}</div>
      </li>)}
    </ol>}
    {details.length > 0 && <details className="conversation-card-details">
      <summary>{t.conversationOriginal}</summary>
      {details.map((content) => <pre key={content}>{content}</pre>)}
    </details>}
  </section>;
}
