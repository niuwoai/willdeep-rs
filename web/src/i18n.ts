export const languages = ["zh-CN", "en", "ja"] as const;
export type Language = (typeof languages)[number];

const zhCN = {
  documentTitle: "WillDeep Web", appName: "WillDeep", webHarness: "Web Harness",
  workspace: "工作区", session: "历史会话", newSession: "新会话", language: "语言",
  languageName: "简体中文", welcomeTitle: "准备好了，从哪里开始？",
  welcomeBody: "选择一个工作区，直接描述你想实现、修复或调查的事情。",
  promptPlaceholder: "描述你想完成的任务…", send: "发送", working: "正在处理",
  loadFailed: "加载失败", requestFailed: "请求失败", noSessions: "这个工作区还没有会话",
  emptyReply: "Harness 没有返回文本",
};

export type Messages = typeof zhCN;

const en: Messages = {
  documentTitle: "WillDeep Web", appName: "WillDeep", webHarness: "Web Harness",
  workspace: "Workspace", session: "Sessions", newSession: "New session", language: "Language",
  languageName: "English", welcomeTitle: "Ready. Where should we start?",
  welcomeBody: "Choose a workspace and describe what you want to build, fix, or investigate.",
  promptPlaceholder: "Describe the task you want to complete…", send: "Send", working: "Working",
  loadFailed: "Failed to load", requestFailed: "Request failed", noSessions: "No sessions in this workspace",
  emptyReply: "The harness returned no text",
};

const ja: Messages = {
  documentTitle: "WillDeep Web", appName: "WillDeep", webHarness: "Web Harness",
  workspace: "ワークスペース", session: "履歴", newSession: "新しいセッション", language: "言語",
  languageName: "日本語", welcomeTitle: "準備できました。どこから始めますか？",
  welcomeBody: "ワークスペースを選び、実装、修正、調査したいことを入力してください。",
  promptPlaceholder: "完了したいタスクを入力…", send: "送信", working: "処理中",
  loadFailed: "読み込みに失敗しました", requestFailed: "リクエストに失敗しました", noSessions: "このワークスペースには履歴がありません",
  emptyReply: "Harness からテキストが返されませんでした",
};

export const messages: Record<Language, Messages> = { "zh-CN": zhCN, en, ja };
export const languageLabels: Record<Language, string> = {
  "zh-CN": zhCN.languageName, en: en.languageName, ja: ja.languageName,
};

export function detectLanguage(): Language {
  const saved = localStorage.getItem("willdeep.language");
  if (languages.includes(saved as Language)) return saved as Language;
  const browser = navigator.language.toLowerCase();
  if (browser.startsWith("ja")) return "ja";
  if (browser.startsWith("en")) return "en";
  return "zh-CN";
}
