type ActivityStep = { label: string; status: "active" | "done" | "failed" };

/** 运行卡片已展示的当前状态，无需在输入框上方再播报一次。 */
export function needsActivityStrip(activity: string, steps: ActivityStep[], thinking: string): boolean {
  return !steps.some((step, index) => {
    if (step.status !== "active" && index !== steps.length - 1) return false;
    return step.label === activity
      || (activity === thinking && step.label.startsWith(`${thinking} ·`));
  });
}
