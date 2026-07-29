/**
 * 内联 SVG 图标 —— 红线：无 emoji chrome。
 * 16×16 viewBox 24，stroke=currentColor，几何极简风。
 */
import type { SVGProps } from "react";

type P = SVGProps<SVGSVGElement>;

const base = (props: P): P => ({
  width: 16,
  height: 16,
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.7,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
  "aria-hidden": true,
  ...props,
});

export const IconBrand = (p: P) => (
  <svg {...base(p)}>
    <circle cx="12" cy="12" r="8.5" />
    <path d="M12 3.5v17M3.5 12h17" opacity={0.4} />
    <circle cx="12" cy="12" r="3" fill="currentColor" stroke="none" />
  </svg>
);

export const IconHome = (p: P) => (
  <svg {...base(p)}>
    <path d="M4 11.5 12 4l8 7.5" />
    <path d="M6.5 10v9h11v-9" />
  </svg>
);

export const IconDeck = (p: P) => (
  <svg {...base(p)}>
    <circle cx="12" cy="12" r="8.5" />
    <circle cx="12" cy="12" r="3.5" />
    <circle cx="12" cy="12" r="0.5" fill="currentColor" />
  </svg>
);

export const IconSessions = (p: P) => (
  <svg {...base(p)}>
    <rect x="4" y="4.5" width="16" height="4.5" rx="1.5" />
    <rect x="4" y="11" width="16" height="4.5" rx="1.5" />
    <rect x="4" y="17.5" width="10" height="4" rx="1.5" />
  </svg>
);

export const IconInbox = (p: P) => (
  <svg {...base(p)}>
    <path d="M4 13.5 6 5.5h12l2 8" />
    <path d="M4 13.5h5l1.5 2.5h3L15 13.5h5v5H4z" />
  </svg>
);

export const IconProjects = (p: P) => (
  <svg {...base(p)}>
    <path d="M4 7.5A1.5 1.5 0 0 1 5.5 6h4L12 8.5h6.5A1.5 1.5 0 0 1 20 10v8.5a1.5 1.5 0 0 1-1.5 1.5h-13A1.5 1.5 0 0 1 4 18.5z" />
  </svg>
);

export const IconFolderOpen = (p: P) => (
  <svg {...base(p)}>
    <path d="M4.5 10.5V7.8A1.8 1.8 0 0 1 6.3 6h3.4L12 8.4h5.8A1.7 1.7 0 0 1 19.5 10v.5" />
    <path d="M3.5 10.5h17l-2 7.3a1.7 1.7 0 0 1-1.7 1.2H5.7A1.7 1.7 0 0 1 4 17.7z" />
  </svg>
);

export const IconMessageCircle = (p: P) => (
  <svg {...base(p)}>
    <path d="M20 11.5a7.7 7.7 0 0 1-8 7.5 9.2 9.2 0 0 1-3.1-.5L4.5 20l1.3-3.7A7.2 7.2 0 0 1 4 11.5 7.7 7.7 0 0 1 12 4a7.7 7.7 0 0 1 8 7.5Z" />
  </svg>
);

export const IconSearch = (p: P) => (
  <svg {...base(p)}>
    <circle cx="10.5" cy="10.5" r="6" />
    <path d="m15.2 15.2 4.3 4.3" />
  </svg>
);

export const IconEditor = (p: P) => (
  <svg {...base(p)}>
    <path d="m8 6-5 6 5 6M16 6l5 6-5 6" />
  </svg>
);

export const IconSettings = (p: P) => (
  <svg {...base(p)}>
    <circle cx="12" cy="12" r="3" />
    <path d="M12 2.8v2.7M12 18.5v2.7M2.8 12h2.7M18.5 12h2.7M5.5 5.5l1.9 1.9M16.6 16.6l1.9 1.9M18.5 5.5l-1.9 1.9M7.4 16.6l-1.9 1.9" />
  </svg>
);

export const IconSend = (p: P) => (
  <svg {...base(p)}>
    <path d="M12 19V5M5.5 11.5 12 5l6.5 6.5" />
  </svg>
);

export const IconPlus = (p: P) => (
  <svg {...base(p)}>
    <path d="M12 5v14M5 12h14" />
  </svg>
);

export const IconArchive = (p: P) => (
  <svg {...base(p)}>
    <path d="M4.5 8h15v11.5H4.5z" />
    <path d="M3.5 5h17v3h-17zM9 12h6" />
  </svg>
);

export const IconTrash = (p: P) => (
  <svg {...base(p)}>
    <path d="M5.5 7.5h13M9 7.5V5h6v2.5M7.5 7.5l.8 12h7.4l.8-12M10 11v5M14 11v5" />
  </svg>
);

export const IconAttach = (p: P) => (
  <svg {...base(p)}>
    <path d="M20 11.5 12.5 19a5 5 0 0 1-7-7l7.5-7.5a3.4 3.4 0 0 1 4.8 4.8L10.5 16.6a1.8 1.8 0 0 1-2.6-2.6l6.8-6.8" />
  </svg>
);

export const IconStop = (p: P) => (
  <svg {...base(p)}>
    <rect x="6.5" y="6.5" width="11" height="11" rx="1.5" fill="currentColor" stroke="none" />
  </svg>
);

export const IconEye = (p: P) => (
  <svg {...base(p)}>
    <path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12Z" />
    <circle cx="12" cy="12" r="2.8" />
  </svg>
);

export const IconCheck = (p: P) => (
  <svg {...base(p)}>
    <path d="m5 12.5 4.5 4.5L19 7.5" />
  </svg>
);

export const IconTerminal = (p: P) => (
  <svg {...base(p)}>
    <rect x="3" y="4.5" width="18" height="15" rx="2" />
    <path d="m7 9.5 3 2.5-3 2.5M12.5 15H17" />
  </svg>
);

export const IconSun = (p: P) => (
  <svg {...base(p)}>
    <circle cx="12" cy="12" r="4" />
    <path d="M12 2.5v2M12 19.5v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2.5 12h2M19.5 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
  </svg>
);

export const IconMoon = (p: P) => (
  <svg {...base(p)}>
    <path d="M20 13.5A8 8 0 1 1 10.5 4a6.5 6.5 0 0 0 9.5 9.5Z" />
  </svg>
);

export const IconMinimize = (p: P) => (
  <svg {...base(p)}>
    <path d="M6 12h12" />
  </svg>
);

export const IconMaximize = (p: P) => (
  <svg {...base(p)}>
    <rect x="6.5" y="6.5" width="11" height="11" rx="1.5" />
  </svg>
);

export const IconClose = (p: P) => (
  <svg {...base(p)}>
    <path d="m6.5 6.5 11 11M17.5 6.5l-11 11" />
  </svg>
);

export const IconChevronDown = (p: P) => (
  <svg {...base(p)}>
    <path d="m6 9.5 6 6 6-6" />
  </svg>
);

export const IconFile = (p: P) => (
  <svg {...base(p)}>
    <path d="M6 3.5h7.5L19 9v11.5H6z" />
    <path d="M13.5 3.5V9H19" />
  </svg>
);

export const IconText = (p: P) => (
  <svg {...base(p)}>
    <path d="M5 7h14M5 12h14M5 17h8" />
  </svg>
);

export const IconAlert = (p: P) => (
  <svg {...base(p)}>
    <path d="M12 4 21 19.5H3z" />
    <path d="M12 10v4.5M12 17.2v.3" />
  </svg>
);

export const IconActivity = (p: P) => (
  <svg {...base(p)}>
    <path d="M4 19.5V10M10 19.5V4.5M16 19.5v-6M22 19.5H2" />
  </svg>
);

export const IconHistory = (p: P) => (
  <svg {...base(p)}>
    <path d="M4.5 8.5V4.8m0 0h3.7m-3.7 0A8.5 8.5 0 1 1 3.8 15" />
    <path d="M12 7v5l3.2 2" />
  </svg>
);

export const IconBell = (p: P) => (
  <svg {...base(p)}>
    <path d="M18 10a6 6 0 0 0-12 0c0 7-2.5 7-2.5 8.8h17C20.5 17 18 17 18 10Z" />
    <path d="M10 21h4" />
  </svg>
);

export const IconHelp = (p: P) => (
  <svg {...base(p)}>
    <circle cx="12" cy="12" r="8.5" />
    <path d="M9.5 9.4a2.7 2.7 0 1 1 4.5 2c-.9.7-2 1.2-2 2.8M12 17.3v.1" />
  </svg>
);

export const IconChevronRight = (p: P) => (
  <svg {...base(p)}>
    <path d="m9.5 6 6 6-6 6" />
  </svg>
);

export const IconChevronLeft = (p: P) => (
  <svg {...base(p)}>
    <path d="m14.5 6-6 6 6 6" />
  </svg>
);

export const IconSidebar = (p: P) => (
  <svg {...base(p)}>
    <rect x="3.5" y="4.5" width="17" height="15" rx="1.8" />
    <path d="M9.5 4.5v15" />
  </svg>
);

export const IconShield = (p: P) => (
  <svg {...base(p)}>
    <path d="M12 3.5 19 6v5.4c0 4.2-2.7 7-7 9.1-4.3-2.1-7-4.9-7-9.1V6z" />
    <path d="m8.8 12 2.1 2.1 4.4-4.4" />
  </svg>
);

export const IconUser = (p: P) => (
  <svg {...base(p)}>
    <circle cx="12" cy="8" r="3.2" />
    <path d="M5.5 20a6.5 6.5 0 0 1 13 0" />
  </svg>
);

export const IconMore = (p: P) => (
  <svg {...base(p)}>
    <circle cx="5" cy="12" r="1" fill="currentColor" stroke="none" />
    <circle cx="12" cy="12" r="1" fill="currentColor" stroke="none" />
    <circle cx="19" cy="12" r="1" fill="currentColor" stroke="none" />
  </svg>
);

export const IconRefresh = (p: P) => (
  <svg {...base(p)}>
    <path d="M19.2 8.6A7.6 7.6 0 0 0 5.5 7L3.8 9.3M4.8 15.4A7.6 7.6 0 0 0 18.5 17l1.7-2.3" />
    <path d="M3.8 5.3v4h4M20.2 18.7v-4h-4" />
  </svg>
);

export const IconArrowRight = (p: P) => (
  <svg {...base(p)}>
    <path d="M4 12h15M14 6.5l5.5 5.5-5.5 5.5" />
  </svg>
);
