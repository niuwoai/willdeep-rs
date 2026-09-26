// 插件页面的主题变量。
//
// macOS 宿主在每个插件页面的 <head> 里注入同一组 CSS 变量（Xedit
// `AgentPluginPageHost.composedHTML`），插件包按这组名字取色：
//
//   --willdeep-bg / --willdeep-fg / --willdeep-secondary / --willdeep-accent
//   --willdeep-body-font-size，外加 :root 的 color-scheme
//
// Web 宿主的值从主界面自己的 token 取，而不是抄 macOS 的字面色值：插件页面
// 嵌在这个界面里，底色要和外框接得上。强调色与 macOS 一样取前景色——宿主是
// 单色调，插件不该在一片灰里突然冒出品牌蓝。

import type { ColorScheme } from "./theme";

/** 插件正文字号。macOS 取聊天正文字号，缺省 14pt；Web 宿主没有这项设置，用同一个缺省值。 */
export const PLUGIN_BODY_FONT_SIZE_PX = 14;

/** 插件变量名 → 取值用的宿主 token。 */
export const PLUGIN_THEME_TOKENS = {
  "--willdeep-bg": "--bg-surface",
  "--willdeep-fg": "--text",
  "--willdeep-secondary": "--text-dim",
  "--willdeep-accent": "--text-strong",
} as const;

export type PluginThemeVariables = Record<keyof typeof PLUGIN_THEME_TOKENS | "--willdeep-body-font-size", string>;

export type PluginTheme = { colorScheme: ColorScheme; variables: PluginThemeVariables };

/** 读根元素上某个 token 的当前值。 */
export function readRootToken(name: string): string {
  if (typeof document === "undefined") return "";
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/** 组装一份要推给插件页面的主题。`readToken` 可替换，便于测试。 */
export function pluginTheme(colorScheme: ColorScheme, readToken: (name: string) => string = readRootToken): PluginTheme {
  const variables = { "--willdeep-body-font-size": `${PLUGIN_BODY_FONT_SIZE_PX}px` } as PluginThemeVariables;
  for (const [variable, token] of Object.entries(PLUGIN_THEME_TOKENS) as Array<[keyof typeof PLUGIN_THEME_TOKENS, string]>) {
    variables[variable] = readToken(token);
  }
  return { colorScheme, variables };
}
