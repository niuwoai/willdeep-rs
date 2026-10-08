import { useCallback, useRef, useState } from "react";

// 轮次收尾后预测用户的下一句（与 TUI 同一契约）：空输入框里灰字显示，Tab 填入
// 不发送，打字只隐藏，删空重新显示；Esc / 新一轮 / 换会话即清。
//
// 世代号是唯一的时序真相：每次清空或重新发起都换代，晚到的回包对不上代就丢，
// 于是「放弃了不回来」不需要额外状态。
//
// 每条建议的结局都上报给本机反馈账本（docs/FEEDBACK_LEDGER.md）：展示、Tab 采用、
// Esc 放弃、无视另打、被顶掉，以及采用后最终发出去的话。原文与停留时长以服务端
// 记下的为准，这里只回传 id；上报失败静默，不打扰输入框。

type SuggestionResponse = { suggestion: string | null; turn_id: string | null; suggestion_id?: string | null };
// 带上所属会话：换了会话即便没来得及清，也不会把别处的预测显示在这里。
export type InputSuggestion = { sessionId: string; text: string; id: string | null };
export type SuggestionOutcome = "dismissed" | "ignored_typed" | "superseded";
type Tracked = { sessionId: string; id: string };

function report(target: Tracked, signal: string, sent?: string) {
  void fetch(`/api/sessions/${encodeURIComponent(target.sessionId)}/input-suggestion/feedback`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ suggestion_id: target.id, signal, sent: sent ?? null }),
    keepalive: true,
  }).catch(() => {});
}

export function useInputSuggestion() {
  const [suggestion, setSuggestion] = useState<InputSuggestion | null>(null);
  const epochRef = useRef(0);
  // state 是异步的，结局上报要看「此刻」挂着哪条：用 ref 镜像一份。
  const shownRef = useRef<Tracked | null>(null);
  // Tab 采用后移到这里，等真正发送时比较「原样 / 改过 / 重写」。
  const acceptedRef = useRef<Tracked | null>(null);

  const settleShown = useCallback((outcome: SuggestionOutcome) => {
    const shown = shownRef.current;
    shownRef.current = null;
    if (shown) report(shown, outcome);
  }, []);

  const abandonAccepted = useCallback(() => {
    const accepted = acceptedRef.current;
    acceptedRef.current = null;
    if (accepted) report(accepted, "superseded");
  }, []);

  // 发送其他内容才结算「无视另打」；Esc 是「放弃」，新一轮、换会话是「被顶掉」。
  // 只有被顶掉才连带作废已采用的那条：用户可能正在改采用进来的话。
  const clear = useCallback((outcome: SuggestionOutcome = "superseded") => {
    epochRef.current += 1;
    settleShown(outcome);
    if (outcome === "superseded") abandonAccepted();
    setSuggestion(null);
  }, [settleShown, abandonAccepted]);

  // Tab：灰字那条记为采用，改由发送时结算。
  const accept = useCallback(() => {
    epochRef.current += 1;
    const shown = shownRef.current;
    shownRef.current = null;
    if (shown) {
      report(shown, "accepted");
      acceptedRef.current = shown;
    }
    setSuggestion(null);
  }, []);

  // 发送时调用：采用过的建议到这里才见分晓。
  const settleSent = useCallback((sent: string) => {
    const accepted = acceptedRef.current;
    acceptedRef.current = null;
    if (accepted) report(accepted, "sent", sent);
    else if (sent) settleShown("ignored_typed");
  }, [settleShown]);

  // `canShow` 在回包时再判一次：用户可能已经开始打字、贴了附件或开了新一轮。
  const request = useCallback(async (sessionId: string, turnId: string | null, canShow: () => boolean) => {
    epochRef.current += 1;
    const epoch = epochRef.current;
    settleShown("superseded");
    abandonAccepted();
    setSuggestion(null);
    try {
      const response = await fetch(`/api/sessions/${encodeURIComponent(sessionId)}/input-suggestion`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ turn_id: turnId }),
      });
      if (!response.ok) return;
      const body = (await response.json()) as SuggestionResponse;
      if (epoch !== epochRef.current || !body.suggestion || !canShow()) return;
      const id = body.suggestion_id ?? null;
      if (id) {
        shownRef.current = { sessionId, id };
        report(shownRef.current, "shown");
      }
      setSuggestion({ sessionId, text: body.suggestion, id });
    } catch {
      // 预测是装饰：拿不到就当没这回事，不打扰用户。
    }
  }, [settleShown, abandonAccepted]);

  return { suggestion, request, clear, accept, settleSent };
}
