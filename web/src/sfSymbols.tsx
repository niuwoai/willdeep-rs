// SF Symbols 在浏览器里不存在，插件清单里的 `sf:` 图标名需要一份等价的
// 线性图标。这里用 Heroicons 风格的 24×24 stroke 路径手绘对应项——形状对得上
// 语义即可，不追求像素级复刻 Apple 的字形。
//
// 认不出来的名字回落成一个圆点而不是空白：一个没图标的入口仍然要能点，
// 也要让人看出"这里本该有个图标"。

const PATHS: Record<string, string> = {
  "arrow.clockwise": "M4.5 12a7.5 7.5 0 1 1 2.2 5.3M4.5 12V7.5M4.5 12H9",
  "arrow.counterclockwise": "M19.5 12a7.5 7.5 0 1 0-2.2 5.3M19.5 12V7.5M19.5 12H15",
  "arrow.uturn.backward": "M9 14 4 9l5-5M4 9h9a6 6 0 0 1 0 12h-3",
  bookmark: "M6 4.5h12v15l-6-4-6 4v-15Z",
  "bookmark.fill": "M6 4.5h12v15l-6-4-6 4v-15Z",
  "chart.bar": "M4 20V10M10 20V4M16 20v-7M22 20H2",
  checklist: "M9 6h11M9 12h11M9 18h11M3.5 6l1.2 1.2L7 5M3.5 12l1.2 1.2L7 11M3.5 18l1.2 1.2L7 17",
  "checkmark.circle": "M9 12.5l2.2 2.2L15.5 10M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0Z",
  "gamecontroller.fill":
    "M7.5 10.5v3M6 12h3M15.5 11.5h.01M17.5 13.5h.01M6.8 7.5h10.4a4.8 4.8 0 0 1 4.6 6.1l-.7 2.6a2.6 2.6 0 0 1-4.6.9L15 15H9l-1.5 2.1a2.6 2.6 0 0 1-4.6-.9l-.7-2.6a4.8 4.8 0 0 1 4.6-6.1Z",
  gearshape:
    "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Zm7.4-3a7.4 7.4 0 0 0-.1-1.2l2-1.5-2-3.4-2.3 1a7.5 7.5 0 0 0-2-1.2l-.4-2.4h-4l-.4 2.4a7.5 7.5 0 0 0-2 1.2l-2.3-1-2 3.4 2 1.5a7.4 7.4 0 0 0 0 2.4l-2 1.5 2 3.4 2.3-1c.6.5 1.3.9 2 1.2l.4 2.4h4l.4-2.4c.7-.3 1.4-.7 2-1.2l2.3 1 2-3.4-2-1.5c.1-.4.1-.8.1-1.2Z",
  "list.bullet": "M8.5 6h12M8.5 12h12M8.5 18h12M3.8 6h.01M3.8 12h.01M3.8 18h.01",
  pencil: "M16.9 3.8l3.3 3.3L8.4 18.9l-4.2.9.9-4.2L16.9 3.8Z",
  "plus.circle": "M12 8.5v7M8.5 12h7M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0Z",
  "square.and.arrow.up": "M12 15V3.5M8.5 7 12 3.5 15.5 7M5 13v6.5a1.5 1.5 0 0 0 1.5 1.5h11a1.5 1.5 0 0 0 1.5-1.5V13",
  "square.and.pencil": "M17 3.8l3.2 3.2L13 14.2l-4 .8.8-4L17 3.8ZM19 14v5.5a1.5 1.5 0 0 1-1.5 1.5h-11A1.5 1.5 0 0 1 5 19.5v-11A1.5 1.5 0 0 1 6.5 7H12",
  "square.grid.2x2": "M4 4h7v7H4V4Zm9 0h7v7h-7V4ZM4 13h7v7H4v-7Zm9 0h7v7h-7v-7Z",
  "square.grid.3x3.fill":
    "M3.5 3.5h5v5h-5v-5Zm6 0h5v5h-5v-5Zm6 0h5v5h-5v-5Zm-12 6h5v5h-5v-5Zm6 0h5v5h-5v-5Zm6 0h5v5h-5v-5Zm-12 6h5v5h-5v-5Zm6 0h5v5h-5v-5Zm6 0h5v5h-5v-5Z",
  "text.badge.plus": "M3.5 6h11M3.5 12h8M3.5 18h8M18 8.5v7M14.5 12h7",
  trash: "M4.5 6.5h15M9.5 6.5V4h5v2.5M6.5 6.5l1 13h9l1-13M10 10v6M14 10v6",
  "trash.slash": "M4.5 6.5h15M6.5 6.5l1 13h9l.6-8M3 3l18 18",
  "waveform.path.ecg": "M2.5 12h4L9 6l3.5 12L15.5 12h6",
  "x.mark": "M6 6l12 12M18 6L6 18",
};

const FILLED = new Set(["bookmark.fill", "gamecontroller.fill", "square.grid.3x3.fill"]);

export function SfIcon({ name, size = 18 }: { name?: string | null; size?: number }) {
  const symbol = name?.startsWith("sf:") ? name.slice(3) : name ?? "";
  const path = PATHS[symbol];
  if (!path) {
    return (
      <svg width={size} height={size} viewBox="0 0 24 24" aria-hidden="true" focusable="false">
        <circle cx="12" cy="12" r="4" fill="currentColor" opacity="0.55" />
      </svg>
    );
  }
  const filled = FILLED.has(symbol);
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill={filled ? "currentColor" : "none"}
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      <path d={path} />
    </svg>
  );
}
