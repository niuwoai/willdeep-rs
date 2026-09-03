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
