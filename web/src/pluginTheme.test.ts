import { describe, expect, it } from "vitest";
import { PLUGIN_BODY_FONT_SIZE_PX, pluginTheme } from "./pluginTheme";
import { resolveColorScheme } from "./theme";

describe("pluginTheme", () => {
  const tokens: Record<string, string> = {
    "--bg-surface": "#0b1118",
    "--text": "#e7edf4",
    "--text-dim": "#8b99aa",
    "--text-strong": "#f2f6fa",
  };

  it("推给插件页面的变量名与 macOS 宿主注入的完全一致", () => {
    const theme = pluginTheme("dark", (name) => tokens[name] ?? "");
    expect(Object.keys(theme.variables).sort()).toEqual([
      "--willdeep-accent",
      "--willdeep-bg",
      "--willdeep-body-font-size",
      "--willdeep-fg",
      "--willdeep-secondary",
    ]);
  });

  it("取值来自宿主 token，强调色与 macOS 一样取前景色", () => {
    const theme = pluginTheme("dark", (name) => tokens[name] ?? "");
    expect(theme).toEqual({
      colorScheme: "dark",
      variables: {
        "--willdeep-bg": "#0b1118",
        "--willdeep-fg": "#e7edf4",
        "--willdeep-secondary": "#8b99aa",
        "--willdeep-accent": "#f2f6fa",
        "--willdeep-body-font-size": `${PLUGIN_BODY_FONT_SIZE_PX}px`,
      },
    });
  });
});

describe("resolveColorScheme", () => {
  it.each([
    ["system", false, "dark"],
    ["system", true, "light"],
    ["dark", true, "dark"],
    ["light", false, "light"],
  ] as const)("档位 %s、系统偏好浅色=%s → %s", (mode, prefersLight, expected) => {
    expect(resolveColorScheme(mode, prefersLight)).toBe(expected);
  });
});
