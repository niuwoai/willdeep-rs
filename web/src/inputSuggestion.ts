import { useCallback, useRef, useState } from "react";

// 轮次收尾后预测用户的下一句（与 TUI 同一契约）：空输入框里灰字显示，Tab 填入
// 不发送，打字 / Esc / 新一轮 / 换会话即清。只活在内存里：刷新页面就没了。
//
// 世代号是唯一的时序真相：每次清空或重新发起都换代，晚到的回包对不上代就丢，
// 于是「放弃了不回来」不需要额外状态。

type SuggestionResponse = { suggestion: string | null; turn_id: string | null };
// 带上所属会话：换了会话即便没来得及清，也不会把别处的预测显示在这里。
export type InputSuggestion = { sessionId: string; text: string };

export function useInputSuggestion() {
  const [suggestion, setSuggestion] = useState<InputSuggestion | null>(null);
  const epochRef = useRef(0);

  const clear = useCallback(() => {
    epochRef.current += 1;
    setSuggestion(null);
  }, []);

  // `canShow` 在回包时再判一次：用户可能已经开始打字、贴了附件或开了新一轮。
  const request = useCallback(async (sessionId: string, turnId: string | null, canShow: () => boolean) => {
    epochRef.current += 1;
    const epoch = epochRef.current;
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
      setSuggestion({ sessionId, text: body.suggestion });
    } catch {
      // 预测是装饰：拿不到就当没这回事，不打扰用户。
    }
  }, []);

  return { suggestion, request, clear };
}
