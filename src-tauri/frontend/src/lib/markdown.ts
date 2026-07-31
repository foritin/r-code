/* ============================================================================
   零依赖 Markdown 解析器
   ----------------------------------------------------------------------------
   纯函数、无 React 依赖：parseMarkdown(src) -> MdNode[]，渲染层只负责把节点
   变成 React 元素（绝不碰 innerHTML）。

   面向「agent 流式输出」的三个取舍：
   1) 未闭合围栏按代码块处理（closed:false）——写到一半的代码不能退化成段落，
      否则每来一个 token 整段就在段落/代码块之间闪一次。
   2) 段落内的单个换行也产生 break：agent 输出普遍靠换行表达结构，
      CommonMark 的「软换行折叠成空格」在这里是错的手感。
   3) href 在解析阶段就做 scheme 白名单过滤，渲染层拿到的 link 节点一定安全；
      被拒的链接降级为它的文字内容，不生成 <a>。

   有意不实现：setext 标题（--- 一律当水平线）、引用式链接 / 脚注、
   HTML 块与内联 HTML（一律当纯文本）、4 空格缩进代码块（agent 缩进噪声太多）。
   ============================================================================ */

/* ---------------------------------------------------------------- 节点类型 */

export type MdAlign = "left" | "center" | "right" | null;

export interface MdText {
  type: "text";
  value: string;
}
/** 换行（行尾双空格 与 单换行 都归一到这里）。 */
export interface MdBreak {
  type: "break";
}
export interface MdCodeSpan {
  type: "codespan";
  value: string;
}
export interface MdStrong {
  type: "strong";
  children: MdInline[];
}
export interface MdEm {
  type: "em";
  children: MdInline[];
}
export interface MdDel {
  type: "del";
  children: MdInline[];
}
/** href 已通过 scheme 白名单；渲染层可直接用。 */
export interface MdLink {
  type: "link";
  href: string;
  title: string | null;
  children: MdInline[];
}
/** 图片资源；渲染层只会通过受控 IPC 加载本地位图，不直接请求远程 URL。 */
export interface MdImage {
  type: "image";
  href: string;
  alt: string;
}

export type MdInline =
  | MdText
  | MdBreak
  | MdCodeSpan
  | MdStrong
  | MdEm
  | MdDel
  | MdLink
  | MdImage;

export interface MdParagraph {
  type: "paragraph";
  children: MdInline[];
}
export interface MdHeading {
  type: "heading";
  /** 1..6 */
  depth: number;
  children: MdInline[];
}
export interface MdCode {
  type: "code";
  /** 规范化后的小写语言标识；无 info string 时为 null。 */
  lang: string | null;
  value: string;
  /** false = 围栏尚未闭合（流式输出中途）。 */
  closed: boolean;
}
export interface MdHr {
  type: "hr";
}
export interface MdBlockquote {
  type: "blockquote";
  children: MdNode[];
}
export interface MdListItem {
  /** null = 普通列表项；true/false = 任务列表勾选态。 */
  checked: boolean | null;
  children: MdNode[];
}
export interface MdList {
  type: "list";
  ordered: boolean;
  start: number;
  /** 紧凑列表：项内单段落不套 <p>。 */
  tight: boolean;
  items: MdListItem[];
}
export interface MdTableCell {
  children: MdInline[];
}
export interface MdTable {
  type: "table";
  align: MdAlign[];
  header: MdTableCell[];
  rows: MdTableCell[][];
}

export type MdNode =
  | MdParagraph
  | MdHeading
  | MdCode
  | MdHr
  | MdBlockquote
  | MdList
  | MdTable;

/* ------------------------------------------------------------------ 入口 */

/** 嵌套深度上限：防止畸形输入（几百层 `>` ）把调用栈打爆。 */
const MAX_DEPTH = 8;
const TAB_WIDTH = 4;

export function parseMarkdown(src: string): MdNode[] {
  if (!src) return [];
  const lines = src.replace(/\r\n?/g, "\n").split("\n");
  return parseBlocks(lines, 0);
}

/* ------------------------------------------------------------ 行工具函数 */

function isBlank(line: string): boolean {
  return line.trim().length === 0;
}

/** 前导空白占的列宽（tab 按 4 列对齐制表位）。 */
function leadingWidth(line: string): number {
  let w = 0;
  for (let i = 0; i < line.length; i++) {
    const c = line.charAt(i);
    if (c === " ") w += 1;
    else if (c === "\t") w += TAB_WIDTH - (w % TAB_WIDTH);
    else break;
  }
  return w;
}

/** 任意字符串占的列宽（用于算列表项的内容列）。 */
function columnWidth(s: string): number {
  let w = 0;
  for (let i = 0; i < s.length; i++) {
    if (s.charAt(i) === "\t") w += TAB_WIDTH - (w % TAB_WIDTH);
    else w += 1;
  }
  return w;
}

/** 去掉至多 width 列的前导空白。 */
function stripIndent(line: string, width: number): string {
  let w = 0;
  let i = 0;
  while (i < line.length && w < width) {
    const c = line.charAt(i);
    if (c === " ") w += 1;
    else if (c === "\t") w += TAB_WIDTH - (w % TAB_WIDTH);
    else break;
    i++;
  }
  return line.slice(i);
}

/* -------------------------------------------------------------- 块级识别 */

const RE_ATX = /^ {0,3}(#{1,6})(?:[ \t]+([^\n]*?))?[ \t]*$/;
const RE_HR = /^ {0,3}(?:(?:\*[ \t]*){3,}|(?:-[ \t]*){3,}|(?:_[ \t]*){3,})$/;
const RE_QUOTE = /^ {0,3}>/;
const RE_LIST = /^([ \t]*)([-*+]|\d{1,9}[.)])([ \t]+|$)/;
const RE_FENCE_OPEN = /^( {0,3})(`{3,}|~{3,})[ \t]*(.*)$/;
const RE_FENCE_CLOSE = /^ {0,3}(`{3,}|~{3,})[ \t]*$/;
const RE_TABLE_DELIM =
  /^ {0,3}\|?[ \t]*:?-+:?[ \t]*(?:\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*$/;
const RE_TASK = /^\[([ xX])\](?:[ \t]+|$)/;

interface Fence {
  indent: number;
  marker: string;
  info: string;
}

function matchFence(line: string): Fence | null {
  const m = RE_FENCE_OPEN.exec(line);
  if (!m) return null;
  const info = m[3].trim();
  // ``` 的 info string 里不允许出现反引号（否则 `` `a` `` 这类行内代码会被误判）
  if (m[2].charAt(0) === "`" && info.indexOf("`") >= 0) return null;
  return { indent: m[1].length, marker: m[2], info };
}

function isFenceClose(line: string, open: Fence): boolean {
  const m = RE_FENCE_CLOSE.exec(line);
  if (!m) return false;
  return m[1].charAt(0) === open.marker.charAt(0) && m[1].length >= open.marker.length;
}

/** info string -> 语言标识（```rust,no_run 或 ```ts title="a.ts" 都只取头一段）。 */
function langFromInfo(info: string): string | null {
  if (!info) return null;
  const first = info.split(/[\s,{]/)[0];
  const m = /^[A-Za-z0-9_+#.-]+$/.exec(first);
  return m ? first.toLowerCase() : null;
}

function isTableDelim(line: string | undefined): boolean {
  return typeof line === "string" && line.indexOf("-") >= 0 && RE_TABLE_DELIM.test(line);
}

/** 该行是否开启一个新块（用于打断段落 / 引用 / 列表的惰性续行）。 */
function startsNewBlock(lines: string[], i: number): boolean {
  const line = lines[i];
  if (matchFence(line)) return true;
  if (RE_HR.test(line)) return true;
  if (RE_ATX.test(line)) return true;
  if (RE_QUOTE.test(line)) return true;
  if (RE_LIST.test(line) && line.trim().length > 1) return true;
  if (line.indexOf("|") >= 0 && isTableDelim(lines[i + 1])) return true;
  return false;
}

/* -------------------------------------------------------------- 块级解析 */

function parseBlocks(lines: string[], depth: number): MdNode[] {
  const out: MdNode[] = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i];

    if (isBlank(line)) {
      i++;
      continue;
    }

    const fence = matchFence(line);
    if (fence) {
      const body: string[] = [];
      let j = i + 1;
      let closed = false;
      while (j < lines.length) {
        if (isFenceClose(lines[j], fence)) {
          closed = true;
          j++;
          break;
        }
        body.push(stripIndent(lines[j], fence.indent));
        j++;
      }
      out.push({
        type: "code",
        lang: langFromInfo(fence.info),
        value: body.join("\n"),
        closed,
      });
      i = j;
      continue;
    }

    if (RE_HR.test(line)) {
      out.push({ type: "hr" });
      i++;
      continue;
    }

    const atx = RE_ATX.exec(line);
    if (atx) {
      const raw = (atx[2] ?? "").replace(/[ \t]+#+$/, "");
      out.push({
        type: "heading",
        depth: atx[1].length,
        children: parseInline(raw),
      });
      i++;
      continue;
    }

    if (RE_QUOTE.test(line)) {
      const inner: string[] = [];
      let j = i;
      while (j < lines.length) {
        const l = lines[j];
        if (RE_QUOTE.test(l)) {
          inner.push(l.replace(/^ {0,3}> ?/, ""));
          j++;
          continue;
        }
        // 惰性续行：紧跟在引用行后的普通段落行仍属于引用
        if (isBlank(l) || startsNewBlock(lines, j)) break;
        inner.push(l.replace(/^[ \t]+/, ""));
        j++;
      }
      out.push({
        type: "blockquote",
        children:
          depth >= MAX_DEPTH
            ? [{ type: "paragraph", children: parseInline(inner.join("\n")) }]
            : parseBlocks(inner, depth + 1),
      });
      i = j;
      continue;
    }

    if (line.indexOf("|") >= 0 && isTableDelim(lines[i + 1])) {
      const table = parseTable(lines, i);
      if (table) {
        out.push(table.node);
        i = table.next;
        continue;
      }
    }

    if (RE_LIST.test(line)) {
      const list = parseList(lines, i, depth);
      out.push(list.node);
      i = list.next;
      continue;
    }

    // 段落：吃到空行或下一个块起点为止
    const start = i;
    const buf: string[] = [];
    while (i < lines.length) {
      const l = lines[i];
      if (isBlank(l)) break;
      if (i > start && startsNewBlock(lines, i)) break;
      buf.push(l.replace(/^[ \t]+/, ""));
      i++;
    }
    const children = parseInline(buf.join("\n"));
    if (children.length > 0) out.push({ type: "paragraph", children });
  }

  return out;
}

/* ------------------------------------------------------------------ 表格 */

/**
 * 拆分一行的单元格。
 * `\|` 在这一步就还原成字面竖线（GFM 的转义早于行内解析，所以
 * `` `a\|b` `` 这种「代码里的竖线」也能正确显示）；其它反斜杠原样留给行内解析。
 */
function splitRow(line: string): string[] {
  let s = line.trim();
  if (s.startsWith("|")) s = s.slice(1);
  if (s.endsWith("|") && !s.endsWith("\\|")) s = s.slice(0, -1);

  const cells: string[] = [];
  let cur = "";
  for (let i = 0; i < s.length; i++) {
    const c = s.charAt(i);
    if (c === "\\" && i + 1 < s.length) {
      const next = s.charAt(i + 1);
      cur += next === "|" ? next : c + next;
      i++;
      continue;
    }
    if (c === "|") {
      cells.push(cur);
      cur = "";
      continue;
    }
    cur += c;
  }
  cells.push(cur);
  return cells.map((c) => c.trim());
}

function alignOf(spec: string): MdAlign {
  const s = spec.trim();
  const left = s.startsWith(":");
  const right = s.endsWith(":");
  if (left && right) return "center";
  if (right) return "right";
  if (left) return "left";
  return null;
}

function parseTable(lines: string[], start: number): { node: MdTable; next: number } | null {
  const header = splitRow(lines[start]);
  const align = splitRow(lines[start + 1]).map(alignOf);
  if (header.length === 0 || align.length === 0) return null;

  const width = header.length;
  const rows: MdTableCell[][] = [];
  let i = start + 2;
  while (i < lines.length) {
    const l = lines[i];
    if (isBlank(l) || l.indexOf("|") < 0) break;
    if (matchFence(l) || RE_ATX.test(l) || RE_QUOTE.test(l) || RE_HR.test(l)) break;
    const cells = splitRow(l);
    const row: MdTableCell[] = [];
    for (let c = 0; c < width; c++) {
      row.push({ children: parseInline(cells[c] ?? "") });
    }
    rows.push(row);
    i++;
  }

  const alignPadded: MdAlign[] = [];
  for (let c = 0; c < width; c++) alignPadded.push(align[c] ?? null);

  return {
    node: {
      type: "table",
      align: alignPadded,
      header: header.map((h) => ({ children: parseInline(h) })),
      rows,
    },
    next: i,
  };
}

/* ------------------------------------------------------------------ 列表 */

function isOrderedMarker(marker: string): boolean {
  return /\d/.test(marker);
}

function parseList(
  lines: string[],
  start: number,
  depth: number
): { node: MdList; next: number } {
  const first = RE_LIST.exec(lines[start]) as RegExpExecArray;
  const base = leadingWidth(lines[start]);
  const ordered = isOrderedMarker(first[2]);
  const startNum = ordered ? parseInt(first[2], 10) || 1 : 1;

  // 1) 收集整个列表覆盖的行
  const block: string[] = [];
  let i = start;
  let sawBlank = false;
  while (i < lines.length) {
    const line = lines[i];

    if (isBlank(line)) {
      let j = i + 1;
      while (j < lines.length && isBlank(lines[j])) j++;
      if (j >= lines.length) break;
      const nextWidth = leadingWidth(lines[j]);
      const nextMarker = RE_LIST.exec(lines[j]);
      const continues =
        nextWidth > base ||
        (nextMarker !== null &&
          nextWidth >= base &&
          isOrderedMarker(nextMarker[2]) === ordered);
      if (!continues) break;
      for (let k = i; k < j; k++) block.push("");
      sawBlank = true;
      i = j;
      continue;
    }

    const width = leadingWidth(line);
    const marker = RE_LIST.exec(line);

    if (width > base) {
      block.push(line);
      i++;
      continue;
    }
    if (marker && width >= base) {
      // 同层但换了有序/无序 → 另起一个列表
      if (isOrderedMarker(marker[2]) !== ordered) break;
      block.push(line);
      i++;
      continue;
    }
    // 缩进回到基线且不是列表项：只有紧挨着的普通行算惰性续行
    if (sawBlank) break;
    if (startsNewBlock(lines, i)) break;
    block.push(line);
    i++;
  }

  // 2) 按「marker 缩进 < 当前项内容列」切分成项
  const rawItems: string[][] = [];
  let cur: string[] | null = null;
  let contentCol = 0;
  for (const line of block) {
    const marker = isBlank(line) ? null : RE_LIST.exec(line);
    if (marker && (cur === null || leadingWidth(line) < contentCol)) {
      const rest = line.slice(marker[0].length);
      contentCol = isBlank(rest)
        ? columnWidth(marker[1] + marker[2]) + 1
        : columnWidth(marker[0]);
      cur = [rest];
      rawItems.push(cur);
      continue;
    }
    if (cur === null) continue;
    cur.push(isBlank(line) ? "" : stripIndent(line, contentCol));
  }

  // 3) 松散判定：项与项之间出现过空行
  let tight = true;
  for (const item of rawItems) {
    for (let k = 0; k < item.length - 1; k++) {
      if (item[k] === "" && item[k + 1] !== "") tight = false;
    }
    if (item.length > 0 && item[item.length - 1] === "") tight = false;
  }

  const items: MdListItem[] = rawItems.map((item) => {
    let checked: boolean | null = null;
    const head = item[0] ?? "";
    const task = RE_TASK.exec(head);
    if (task) {
      checked = task[1].toLowerCase() === "x";
      item[0] = head.slice(task[0].length);
    }
    const children =
      depth >= MAX_DEPTH
        ? [{ type: "paragraph" as const, children: parseInline(item.join("\n")) }]
        : parseBlocks(item, depth + 1);
    return { checked, children };
  });

  return {
    node: { type: "list", ordered, start: startNum, tight, items },
    next: i,
  };
}

/* ------------------------------------------------------------------ 安全 */

const SAFE_SCHEME = /^(?:https?|mailto|file):/i;
const HAS_SCHEME = /^[A-Za-z][A-Za-z0-9+.-]*:/;
const WINDOWS_ABSOLUTE_PATH = /^[A-Za-z]:[\\/]/;

/** Whether a sanitized destination must be handled by the trusted local-resource bridge. */
export function isLocalResourceUrl(raw: string): boolean {
  const url = raw.trim();
  if (!url) return false;
  const probe = url.replace(/[\u0000-\u0020\u007f\u00a0\u2028\u2029]/g, "");
  if (/^file:/i.test(probe) || WINDOWS_ABSOLUTE_PATH.test(probe)) return true;
  if (HAS_SCHEME.test(probe) || probe.startsWith("//")) return false;
  return true;
}

/**
 * URL/resource whitelist. Web links are limited to http(s)/mailto. Local file URLs, Windows
 * absolute paths, POSIX paths and workspace-relative paths are kept as inert resource references;
 * the renderer never navigates them directly and resolves them through host IPC instead.
 */
export function sanitizeUrl(raw: string): string | null {
  const url = raw.trim();
  if (!url) return null;
  // 去掉控制字符与内嵌空白后再判 scheme，挡住 "java\nscript:" 这类混淆
  const probe = url.replace(/[\u0000-\u0020\u007f\u00a0\u2028\u2029]/g, "");
  if (WINDOWS_ABSOLUTE_PATH.test(probe)) return url;
  if (!HAS_SCHEME.test(probe)) return probe.startsWith("//") ? null : url;
  if (!SAFE_SCHEME.test(probe)) return null;
  return url;
}

/* -------------------------------------------------------------- 行内解析 */

const ESCAPABLE = "\\`*_{}[]()#+-.!|~<>\"'$&,/:;=?@^";

const RE_AUTOLINK =
  /<((?:[A-Za-z][A-Za-z0-9+.-]{1,31}:[^<>\s]*)|(?:[^\s<>@]+@[^\s<>@]+\.[^\s<>@]+))>/y;
/** 只吃 RFC 3986 允许的字符：中文紧跟在 URL 后面（"见 https://a.com的说明"）不会被吞进去。 */
const RE_BARE_URL = /(?:https?:\/\/|www\.)[A-Za-z0-9\-._~:/?#[\]@!$&'()*+,;=%]+/y;

/** 跳过一个行内代码段，返回结束位置（含闭合反引号）；未闭合时返回入参。 */
function skipCodeSpan(src: string, i: number): number {
  let n = 0;
  while (src.charAt(i + n) === "`") n++;
  let j = i + n;
  while (j < src.length) {
    if (src.charAt(j) === "`") {
      let r = 0;
      while (src.charAt(j + r) === "`") r++;
      if (r === n) return j + r;
      j += r;
      continue;
    }
    j++;
  }
  return i;
}

/** 找与 `[` 配对的 `]`（跳过转义与行内代码）。 */
function findBracketEnd(src: string, start: number): number {
  let depth = 0;
  let i = start;
  while (i < src.length) {
    const c = src.charAt(i);
    if (c === "\\") {
      i += 2;
      continue;
    }
    if (c === "`") {
      const end = skipCodeSpan(src, i);
      if (end > i) {
        i = end;
        continue;
      }
    }
    if (c === "[") depth++;
    else if (c === "]") {
      depth--;
      if (depth === 0) return i;
    }
    i++;
  }
  return -1;
}

interface Destination {
  href: string;
  title: string | null;
  next: number;
}

/** 解析 `(url "title")`，i 指向左括号。 */
function parseDestination(src: string, i: number): Destination | null {
  let j = i + 1;
  let depth = 1;
  let raw = "";
  let closed = false;
  while (j < src.length) {
    const c = src.charAt(j);
    if (c === "\\") {
      const next = src.charAt(j + 1);
      // Only consume Markdown destination escapes. Keeping `\\U` intact is essential for native
      // Windows paths such as `C:\\Users\\name\\result.png`.
      if (next === "\\" || next === "(" || next === ")") {
        raw += next;
        j += 2;
      } else {
        raw += c;
        j++;
      }
      continue;
    }
    if (c === "(") {
      depth++;
      raw += c;
      j++;
      continue;
    }
    if (c === ")") {
      depth--;
      if (depth === 0) {
        j++;
        closed = true;
        break;
      }
      raw += c;
      j++;
      continue;
    }
    raw += c === "\n" ? " " : c;
    j++;
  }
  if (!closed) return null;

  let href = raw.trim();
  let title: string | null = null;
  const withTitle = /^(.*?)[ \t]+(?:"([^"]*)"|'([^']*)')$/.exec(href);
  if (withTitle) {
    href = withTitle[1].trim();
    title = withTitle[2] ?? withTitle[3] ?? null;
  }
  if (href.startsWith("<") && href.endsWith(">")) href = href.slice(1, -1);
  return { href, title, next: j };
}

/** 从 from 起找强调的闭合分隔符；返回闭合 run 的起始下标，找不到返回 -1。 */
function findCloser(src: string, from: number, ch: string, len: number): number {
  let i = from;
  while (i < src.length) {
    const c = src.charAt(i);
    if (c === "\\") {
      i += 2;
      continue;
    }
    if (c === "`") {
      const end = skipCodeSpan(src, i);
      if (end > i) {
        i = end;
        continue;
      }
      i++;
      continue;
    }
    if (c === ch) {
      let r = 1;
      while (src.charAt(i + r) === ch) r++;
      const prev = src.charAt(i - 1);
      if (r >= len && i > from && !/\s/.test(prev)) return i;
      i += r;
      continue;
    }
    i++;
  }
  return -1;
}

interface Emphasis {
  node: MdInline;
  next: number;
}

function matchEmphasis(src: string, i: number): Emphasis | null {
  const ch = src.charAt(i);
  if (ch !== "*" && ch !== "_" && ch !== "~") return null;

  let run = 1;
  while (src.charAt(i + run) === ch) run++;

  let len: number;
  if (ch === "~") {
    if (run < 2) return null;
    len = 2;
  } else {
    len = Math.min(run, 3);
  }

  const openEnd = i + len;
  const after = src.charAt(openEnd);
  if (!after || /\s/.test(after)) return null;
  // `_` 不参与词内强调：snake_case_name 必须原样保留
  if (ch === "_" && i > 0 && /[\w]/.test(src.charAt(i - 1))) return null;

  const close = findCloser(src, openEnd, ch, len);
  if (close < 0) return null;
  if (ch === "_" && /[\w]/.test(src.charAt(close + len))) return null;

  const inner = src.slice(openEnd, close);
  if (!inner) return null;
  const children = parseInline(inner);
  if (children.length === 0) return null;

  let node: MdInline;
  if (ch === "~") node = { type: "del", children };
  else if (len === 3) node = { type: "em", children: [{ type: "strong", children }] };
  else if (len === 2) node = { type: "strong", children };
  else node = { type: "em", children };

  return { node, next: close + len };
}

/** 裸 URL 结尾常粘着句读（中英文都要管）；把它们吐回正文。 */
const URL_TAIL_PUNCT = ".,;:!?'\"。，、；：！？…《》「」『』【】〉》—";

function trimUrlTail(url: string): string {
  let end = url.length;
  while (end > 0) {
    const c = url.charAt(end - 1);
    if (URL_TAIL_PUNCT.indexOf(c) >= 0) {
      end--;
      continue;
    }
    if (c === ")") {
      const head = url.slice(0, end);
      const opens = (head.match(/\(/g) ?? []).length;
      const closes = (head.match(/\)/g) ?? []).length;
      if (closes > opens) {
        end--;
        continue;
      }
    }
    break;
  }
  return url.slice(0, end);
}

export function parseInline(src: string): MdInline[] {
  const out: MdInline[] = [];
  let buf = "";

  const flush = (): void => {
    if (buf) {
      out.push({ type: "text", value: buf });
      buf = "";
    }
  };
  const push = (node: MdInline): void => {
    flush();
    out.push(node);
  };

  let i = 0;
  while (i < src.length) {
    const c = src.charAt(i);

    // 转义
    if (c === "\\") {
      const next = src.charAt(i + 1);
      if (next && ESCAPABLE.indexOf(next) >= 0) {
        buf += next;
        i += 2;
        continue;
      }
      buf += c;
      i++;
      continue;
    }

    // 换行 → break（行尾空白丢弃，下一行前导空白也丢弃）
    if (c === "\n") {
      buf = buf.replace(/[ \t]+$/, "");
      push({ type: "break" });
      i++;
      while (i < src.length && (src.charAt(i) === " " || src.charAt(i) === "\t")) i++;
      continue;
    }

    // 行内代码（多反引号形式一并处理）
    if (c === "`") {
      const end = skipCodeSpan(src, i);
      if (end > i) {
        let n = 0;
        while (src.charAt(i + n) === "`") n++;
        let value = src.slice(i + n, end - n).replace(/\n/g, " ");
        if (
          value.length > 1 &&
          value.startsWith(" ") &&
          value.endsWith(" ") &&
          value.trim() !== ""
        ) {
          value = value.slice(1, -1);
        }
        push({ type: "codespan", value });
        i = end;
        continue;
      }
    }

    // 图片 ![alt](url)
    if (c === "!" && src.charAt(i + 1) === "[") {
      const close = findBracketEnd(src, i + 1);
      if (close > 0 && src.charAt(close + 1) === "(") {
        const dest = parseDestination(src, close + 1);
        if (dest) {
          const alt = src.slice(i + 2, close);
          const href = sanitizeUrl(dest.href);
          if (href) push({ type: "image", href, alt });
          else buf += alt;
          i = dest.next;
          continue;
        }
      }
    }

    // 链接 [text](url)
    if (c === "[") {
      const close = findBracketEnd(src, i);
      if (close > 0 && src.charAt(close + 1) === "(") {
        const dest = parseDestination(src, close + 1);
        if (dest) {
          const label = src.slice(i + 1, close);
          const children = parseInline(label);
          const href = sanitizeUrl(dest.href);
          if (href) {
            push({ type: "link", href, title: dest.title, children });
          } else {
            // 被拒的 scheme：降级成链接文字，绝不生成 <a>
            flush();
            for (const child of children) out.push(child);
          }
          i = dest.next;
          continue;
        }
      }
    }

    // 自动链接 <https://...> / <a@b.c>
    if (c === "<") {
      RE_AUTOLINK.lastIndex = i;
      const m = RE_AUTOLINK.exec(src);
      if (m) {
        const target = m[1];
        const raw = target.indexOf("@") >= 0 && !HAS_SCHEME.test(target)
          ? "mailto:" + target
          : target;
        const href = sanitizeUrl(raw);
        if (href) {
          push({ type: "link", href, title: null, children: [{ type: "text", value: target }] });
        } else {
          buf += m[0];
        }
        i = m.index + m[0].length;
        continue;
      }
    }

    // 裸 URL
    if ((c === "h" || c === "w") && !/[\w@/.]/.test(src.charAt(i - 1) || "")) {
      RE_BARE_URL.lastIndex = i;
      const m = RE_BARE_URL.exec(src);
      if (m) {
        const text = trimUrlTail(m[0]);
        if (text.length > 4) {
          const href = sanitizeUrl(text.startsWith("www.") ? "https://" + text : text);
          if (href) {
            push({ type: "link", href, title: null, children: [{ type: "text", value: text }] });
            i += text.length;
            continue;
          }
        }
      }
    }

    // 强调 / 删除线
    if (c === "*" || c === "_" || c === "~") {
      const em = matchEmphasis(src, i);
      if (em) {
        push(em.node);
        i = em.next;
        continue;
      }
    }

    buf += c;
    i++;
  }

  flush();
  return out;
}

/** 取节点的纯文本（复制、title、无障碍标签用）。 */
export function inlineToText(nodes: MdInline[]): string {
  let out = "";
  for (const node of nodes) {
    switch (node.type) {
      case "text":
        out += node.value;
        break;
      case "break":
        out += "\n";
        break;
      case "codespan":
        out += node.value;
        break;
      case "image":
        out += node.alt;
        break;
      case "strong":
      case "em":
      case "del":
      case "link":
        out += inlineToText(node.children);
        break;
    }
  }
  return out;
}
