import { useEffect, useState } from "react";

export const themeModes = ["system", "dark", "light"] as const;
export type ThemeMode = (typeof themeModes)[number];

const STORAGE_KEY = "willdeep.theme";

/// 记住的主题。认不出的值一律回落到跟随系统——存储里的东西可能是旧版本写的，
/// 也可能是人手改的，拿它当真会让界面停在一个谁也没选过的配色上。
export function storedThemeMode(): ThemeMode {
  try {
    const value = localStorage.getItem(STORAGE_KEY);
    return themeModes.includes(value as ThemeMode) ? (value as ThemeMode) : "system";
  } catch {
    // 隐私模式下 localStorage 会直接抛。主题不是关键功能，读不到就跟随系统。
    return "system";
  }
}

/// 把选择写到根元素上，样式表按 `data-theme` 取对应那套变量。
///
/// 跟随系统时**移除**属性而不是写 `system`：那一档由 `prefers-color-scheme`
/// 媒体查询负责，属性留着反而会盖住它。
export function applyThemeMode(mode: ThemeMode) {
  const root = document.documentElement;
  if (mode === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", mode);
  try {
    localStorage.setItem(STORAGE_KEY, mode);
  } catch {
    // 存不下就只影响这一次会话，不该让界面报错。
  }
}

/// 解析之后的配色：插件页面拿到的永远是这两个之一，「跟随系统」由宿主先解析掉。
export type ColorScheme = "light" | "dark";

const PREFERS_LIGHT_QUERY = "(prefers-color-scheme: light)";

/// 纯函数，方便单独推理：固定档直接用，跟随系统那档看系统是否偏好浅色。
/// 与 theme.css 的规则一致——系统不说浅色（包括不支持这条媒体查询）就是深色。
export function resolveColorScheme(mode: ThemeMode, prefersLight: boolean): ColorScheme {
  if (mode === "system") return prefersLight ? "light" : "dark";
  return mode;
}

function systemPrefersLight(): boolean {
  try {
    return window.matchMedia(PREFERS_LIGHT_QUERY).matches;
  } catch {
    return false;
  }
}

/// 当前真正生效的配色。主题设置变了、或者跟随系统时系统切了明暗，都会重新渲染。
export function useResolvedColorScheme(mode: ThemeMode): ColorScheme {
  const [prefersLight, setPrefersLight] = useState(systemPrefersLight);
  useEffect(() => {
    let query: MediaQueryList;
    try {
      query = window.matchMedia(PREFERS_LIGHT_QUERY);
    } catch {
      return undefined;
    }
    const onChange = (event: MediaQueryListEvent) => setPrefersLight(event.matches);
    // 订阅前后系统可能已经切过一次，先对齐一下。
    setPrefersLight(query.matches);
    query.addEventListener("change", onChange);
    return () => query.removeEventListener("change", onChange);
  }, []);
  return resolveColorScheme(mode, prefersLight);
}
