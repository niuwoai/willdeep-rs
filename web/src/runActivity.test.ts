import { describe, expect, it } from "vitest";
import { messages } from "./i18n";
import { needsActivityStrip } from "./runActivity";

describe("运行状态去重", () => {
  for (const [language, t] of Object.entries(messages)) {
    it(`${language}：思考步骤和推理流只显示一份状态`, () => {
      const label = `${t.thinking} · 2`;
      const steps = [{ label, status: "active" as const }];
      expect(needsActivityStrip(label, steps, t.thinking)).toBe(false);
      expect(needsActivityStrip(t.thinking, steps, t.thinking)).toBe(false);
    });

    it(`${language}：保留启动、重连与停止状态`, () => {
      expect(needsActivityStrip(t.thinking, [], t.thinking)).toBe(true);
      const steps = [{ label: `${t.thinking} · 2`, status: "active" as const }];
      expect(needsActivityStrip(t.reconnecting, steps, t.thinking)).toBe(true);
      expect(needsActivityStrip(t.stopping, steps, t.thinking)).toBe(true);
    });
  }

  it("工具执行和结束状态去重，独立的重试提示保留", () => {
    const steps = [{ label: "tool", status: "active" as const }];
    expect(needsActivityStrip("tool", steps, "thinking")).toBe(false);
    expect(needsActivityStrip("retry", steps, "thinking")).toBe(true);
    expect(needsActivityStrip("done", [{ label: "done", status: "done" }], "thinking")).toBe(false);
  });

  it("旧步骤不遮挡新的状态提示", () => {
    expect(needsActivityStrip("thinking", [
      { label: "thinking", status: "done" },
      { label: "tool", status: "done" },
    ], "thinking")).toBe(true);
  });
});
