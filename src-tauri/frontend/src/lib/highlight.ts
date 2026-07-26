/* ============================================================================
   零依赖语法高亮 —— 正则词法着色（不是解析器，也不打算是）
   ----------------------------------------------------------------------------
   highlight(code, lang) -> Token[]，每个 token 带一个统一的 class：
     tok-kw / tok-str / tok-num / tok-com / tok-fn / tok-type / tok-punc /
     tok-attr / tok-add / tok-del
   未知语言或无 info string → 单个无 class 的 token（不瞎猜）。

   实现要点：规则表按优先级排列，用 sticky(y) 正则在当前位置逐个试匹配，
   第一个命中的规则赢。注释与字符串排在关键字之前，所以 `// let x` 里的 let
   不会被着色；同理字符串里的 # 不会被当成注释。带 ^ 的规则额外挂 m 标志，
   靠「换行单独成 token」保证 lastIndex 落在行首时断言仍然成立。
   ============================================================================ */

export interface Token {
  text: string;
  /** null = 普通文本 */
  cls: string | null;
}

interface Rule {
  re: RegExp;
  cls: string | null;
}

/** 超大代码块不值得为着色付出 CPU（流式下每帧都会重算）。 */
const MAX_HIGHLIGHT_CHARS = 120_000;

/* ---------------------------------------------------------------- 通用片段 */

const WS: Rule = { re: /[^\S\n]+/y, cls: null };
const NL: Rule = { re: /\n/y, cls: null };
const IDENT: Rule = { re: /[A-Za-z_$][A-Za-z0-9_$]*/y, cls: null };
const PUNCT: Rule = { re: /[{}()[\].,;:!?<>=+\-*/%&|^~@#\\]+/y, cls: "tok-punc" };
const NUM: Rule = {
  re: /(?:0[xX][0-9a-fA-F_]+|0[bB][01_]+|0[oO][0-7_]+|(?:\d[\d_]*)?\.?\d[\d_]*(?:[eE][+-]?\d+)?)(?:[A-Za-z_][A-Za-z0-9_]*)?/y,
  cls: "tok-num",
};
const CALL: Rule = { re: /[A-Za-z_$][A-Za-z0-9_$]*(?=[ \t]*\()/y, cls: "tok-fn" };
const UPPER: Rule = { re: /[A-Z][A-Za-z0-9_]*/y, cls: "tok-type" };

const LINE_COMMENT_SLASH: Rule = { re: /\/\/[^\n]*/y, cls: "tok-com" };
const BLOCK_COMMENT: Rule = { re: /\/\*[\s\S]*?(?:\*\/|$)/y, cls: "tok-com" };
const HASH_COMMENT: Rule = { re: /#[^\n]*/y, cls: "tok-com" };

/** 单行字符串：先吃闭合的，再兜底未闭合的（流式代码常写到一半）。 */
function strRules(quotes: string, multiline = false): Rule[] {
  const rules: Rule[] = [];
  for (const q of quotes) {
    const esc = q === "'" ? "'" : q === '"' ? '"' : "`";
    const body = multiline ? `(?:\\\\[\\s\\S]|[^${esc}\\\\])*` : `(?:\\\\.|[^${esc}\\\\\\n])*`;
    rules.push({ re: new RegExp(`${esc}${body}${esc}`, "y"), cls: "tok-str" });
    rules.push({ re: new RegExp(`${esc}[^\\n]*`, "y"), cls: "tok-str" });
  }
  return rules;
}

function kw(words: string): RegExp {
  return new RegExp(`(?:${words.trim().split(/\s+/).join("|")})\\b`, "y");
}

/* ------------------------------------------------------------------ Rust */

const RUST: Rule[] = [
  { re: /\/\/\/?[^\n]*/y, cls: "tok-com" },
  BLOCK_COMMENT,
  { re: /b?r#*"[\s\S]*?"#*/y, cls: "tok-str" },
  ...strRules('"', true),
  { re: /'(?:\\[\s\S]|[^'\\])'/y, cls: "tok-str" },
  { re: /'[A-Za-z_][A-Za-z0-9_]*/y, cls: "tok-type" },
  { re: /#!?\[[^\]\n]*\]/y, cls: "tok-attr" },
  {
    re: kw(
      "as async await box break const continue crate dyn else enum extern false fn for " +
        "if impl in let loop macro_rules match mod move mut pub ref return self Self static " +
        "struct super trait true type union unsafe use where while yield"
    ),
    cls: "tok-kw",
  },
  {
    re: kw(
      "i8 i16 i32 i64 i128 isize u8 u16 u32 u64 u128 usize f32 f64 bool char str " +
        "String Vec Option Result Box Rc Arc RefCell HashMap HashSet BTreeMap Cow"
    ),
    cls: "tok-type",
  },
  NUM,
  { re: /[A-Za-z_][A-Za-z0-9_]*!/y, cls: "tok-fn" },
  CALL,
  UPPER,
  IDENT,
  WS,
  NL,
  PUNCT,
];

/* ------------------------------------------------------- TypeScript / JS */

const TS: Rule[] = [
  LINE_COMMENT_SLASH,
  BLOCK_COMMENT,
  ...strRules("`", true),
  ...strRules("\"'"),
  {
    re: kw(
      "abstract as async await break case catch class const continue debugger declare " +
        "default delete do else enum export extends false finally for from function get " +
        "if implements import in infer instanceof interface is keyof let namespace new null " +
        "of override private protected public readonly return satisfies set static super " +
        "switch this throw true try type typeof undefined var void while with yield"
    ),
    cls: "tok-kw",
  },
  {
    re: kw(
      "string number boolean object symbol bigint any unknown never void Array Promise " +
        "Record Partial Readonly Map Set Date RegExp Error JSON Math console"
    ),
    cls: "tok-type",
  },
  NUM,
  CALL,
  UPPER,
  IDENT,
  WS,
  NL,
  PUNCT,
];

/* ---------------------------------------------------------------- Python */

const PYTHON: Rule[] = [
  HASH_COMMENT,
  { re: /[fFrRbBuU]{0,2}"""[\s\S]*?(?:"""|$)/y, cls: "tok-str" },
  { re: /[fFrRbBuU]{0,2}'''[\s\S]*?(?:'''|$)/y, cls: "tok-str" },
  { re: /[fFrRbBuU]{0,2}"(?:\\.|[^"\\\n])*"/y, cls: "tok-str" },
  { re: /[fFrRbBuU]{0,2}'(?:\\.|[^'\\\n])*'/y, cls: "tok-str" },
  ...strRules("\"'"),
  { re: /@[A-Za-z_][A-Za-z0-9_.]*/y, cls: "tok-attr" },
  {
    re: kw(
      "and as assert async await break class continue def del elif else except finally " +
        "for from global if import in is lambda match case nonlocal not or pass raise " +
        "return try while with yield None True False self cls"
    ),
    cls: "tok-kw",
  },
  {
    re: kw(
      "int float str bool bytes list dict set tuple frozenset object type " +
        "Exception ValueError TypeError KeyError IndexError RuntimeError " +
        "print len range enumerate zip open isinstance super"
    ),
    cls: "tok-type",
  },
  NUM,
  CALL,
  UPPER,
  IDENT,
  WS,
  NL,
  PUNCT,
];

/* ------------------------------------------------------------------ JSON */

const JSON_RULES: Rule[] = [
  { re: /"(?:\\.|[^"\\])*"(?=[ \t]*:)/y, cls: "tok-attr" },
  { re: /"(?:\\.|[^"\\])*"/y, cls: "tok-str" },
  { re: /"[^\n]*/y, cls: "tok-str" },
  { re: kw("true false null"), cls: "tok-kw" },
  { re: /-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/y, cls: "tok-num" },
  LINE_COMMENT_SLASH,
  BLOCK_COMMENT,
  IDENT,
  WS,
  NL,
  PUNCT,
];

/* ------------------------------------------------------------------ Bash */

const BASH: Rule[] = [
  HASH_COMMENT,
  { re: /\$'(?:\\.|[^'\\\n])*'/y, cls: "tok-str" },
  ...strRules("\"'"),
  { re: /\$\{[^}\n]*\}|\$[A-Za-z_][A-Za-z0-9_]*|\$[0-9@*#?$!-]/y, cls: "tok-attr" },
  {
    re: kw(
      "if then else elif fi for while until do done case esac in function select " +
        "return break continue local export readonly declare typeset source alias " +
        "unalias unset shift trap set eval exec exit time coproc"
    ),
    cls: "tok-kw",
  },
  {
    re: kw(
      "echo printf cd ls cat head tail grep sed awk find mkdir rmdir rm cp mv ln touch " +
        "chmod chown kill ps sleep sort uniq wc xargs tee curl wget git npm npx pnpm yarn " +
        "cargo rustc node python python3 pip pipx docker kubectl make cmake tar zip unzip sudo which env"
    ),
    cls: "tok-fn",
  },
  { re: /--?[A-Za-z][A-Za-z0-9-]*/y, cls: "tok-attr" },
  NUM,
  { re: /[A-Za-z_][A-Za-z0-9_.\-/]*/y, cls: null },
  WS,
  NL,
  PUNCT,
];

/* ------------------------------------------------------------------ TOML */

const TOML: Rule[] = [
  HASH_COMMENT,
  { re: /^[ \t]*\[\[?[^\]\n]*\]\]?/my, cls: "tok-type" },
  { re: /"""[\s\S]*?(?:"""|$)/y, cls: "tok-str" },
  { re: /'''[\s\S]*?(?:'''|$)/y, cls: "tok-str" },
  { re: /(?:"(?:\\.|[^"\\\n])*"|'[^'\n]*')(?=[ \t]*=)/y, cls: "tok-attr" },
  { re: /[A-Za-z_][A-Za-z0-9_.-]*(?=[ \t]*=)/y, cls: "tok-attr" },
  ...strRules("\"'"),
  { re: kw("true false"), cls: "tok-kw" },
  { re: /\d{4}-\d{2}-\d{2}(?:[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})?)?/y, cls: "tok-num" },
  NUM,
  IDENT,
  WS,
  NL,
  PUNCT,
];

/* ------------------------------------------------------------------ YAML */

const YAML: Rule[] = [
  HASH_COMMENT,
  { re: /^[ \t]*(?:-[ \t]+)*(?:[A-Za-z_][\w.\-/]*|"[^"\n]*"|'[^'\n]*')(?=[ \t]*:(?:[ \t]|$))/my, cls: "tok-attr" },
  { re: /^[ \t]*-{3}[ \t]*$/my, cls: "tok-punc" },
  ...strRules("\"'"),
  { re: /[&*][A-Za-z_][\w.-]*/y, cls: "tok-type" },
  { re: /![A-Za-z_][\w.:-]*/y, cls: "tok-type" },
  { re: /(?:true|false|null|yes|no|on|off|~)\b/iy, cls: "tok-kw" },
  { re: /\d{4}-\d{2}-\d{2}(?:[T ][\d:.+-]+)?/y, cls: "tok-num" },
  NUM,
  { re: /[A-Za-z_][\w.\-/]*/y, cls: null },
  WS,
  NL,
  PUNCT,
];

/* -------------------------------------------------------------- HTML/XML */

const HTML: Rule[] = [
  { re: /<!--[\s\S]*?(?:-->|$)/y, cls: "tok-com" },
  { re: /<!\[CDATA\[[\s\S]*?(?:\]\]>|$)/y, cls: "tok-com" },
  { re: /<[!?][A-Za-z][^>\n]*>?/y, cls: "tok-kw" },
  { re: /<\/?[A-Za-z][\w:.-]*/y, cls: "tok-kw" },
  { re: /\/?>/y, cls: "tok-punc" },
  { re: /[A-Za-z_:@#[][\w:.\-\]]*(?=[ \t]*=)/y, cls: "tok-attr" },
  ...strRules("\"'"),
  { re: /&[#\w]+;/y, cls: "tok-num" },
  NUM,
  IDENT,
  WS,
  NL,
  PUNCT,
];

/* ------------------------------------------------------------------- CSS */

/** CSS 的标点表要把 `-` 排除掉，否则贪婪匹配会把 `var(--x)` 的 `(--` 吞成一个标点。 */
const CSS_PUNCT: Rule = { re: /[{}()[\].,;:!?<>=+*/%&|^~#\\]+/y, cls: "tok-punc" };

const CSS: Rule[] = [
  BLOCK_COMMENT,
  ...strRules("\"'"),
  { re: /@[A-Za-z-]+/y, cls: "tok-kw" },
  { re: /!important\b/y, cls: "tok-kw" },
  { re: /--[A-Za-z0-9_-]+/y, cls: "tok-attr" },
  { re: /#[0-9a-fA-F]{3,8}\b/y, cls: "tok-num" },
  { re: /[-A-Za-z][A-Za-z0-9-]*(?=[ \t]*:)/y, cls: "tok-attr" },
  { re: /[-A-Za-z][A-Za-z0-9-]*(?=\()/y, cls: "tok-fn" },
  { re: /[.#][A-Za-z_][A-Za-z0-9_-]*/y, cls: "tok-type" },
  { re: /::?[A-Za-z-]+/y, cls: "tok-type" },
  { re: /\[[^\]\n]*\]/y, cls: "tok-type" },
  { re: /(?:\d+\.?\d*|\.\d+)(?:%|[A-Za-z]+)?/y, cls: "tok-num" },
  { re: /[A-Za-z_][A-Za-z0-9_-]*/y, cls: null },
  WS,
  NL,
  CSS_PUNCT,
];

/* ------------------------------------------------------------------- SQL */

const SQL: Rule[] = [
  { re: /--[^\n]*/y, cls: "tok-com" },
  BLOCK_COMMENT,
  { re: /'(?:''|[^'])*'/y, cls: "tok-str" },
  { re: /'[^\n]*/y, cls: "tok-str" },
  { re: /"(?:""|[^"])*"|`[^`\n]*`/y, cls: "tok-str" },
  {
    re: new RegExp(
      "(?:" +
        (
          "select from where insert into values update set delete create table alter drop " +
          "index view materialized join left right full inner outer cross on using group by " +
          "order having limit offset union all except intersect as distinct and or not null " +
          "is in like ilike between case when then else end primary key foreign references " +
          "default constraint unique check cascade returning with recursive exists begin " +
          "commit rollback transaction grant revoke explain analyze database schema if"
        )
          .trim()
          .split(/\s+/)
          .join("|") +
        ")\\b",
      "iy"
    ),
    cls: "tok-kw",
  },
  {
    re: new RegExp(
      "(?:" +
        "int integer bigint smallint serial bigserial text varchar char boolean bool date timestamp timestamptz numeric decimal real double float json jsonb uuid bytea array"
          .split(/\s+/)
          .join("|") +
        ")\\b",
      "iy"
    ),
    cls: "tok-type",
  },
  NUM,
  CALL,
  { re: /[A-Za-z_][A-Za-z0-9_.$]*/y, cls: null },
  WS,
  NL,
  PUNCT,
];

/* -------------------------------------------------------------------- Go */

const GO: Rule[] = [
  LINE_COMMENT_SLASH,
  BLOCK_COMMENT,
  { re: /`[^`]*`?/y, cls: "tok-str" },
  ...strRules('"'),
  { re: /'(?:\\[\s\S]|[^'\\])'/y, cls: "tok-str" },
  {
    re: kw(
      "break case chan const continue default defer else fallthrough for func go goto " +
        "if import interface map package range return select struct switch type var " +
        "nil true false iota"
    ),
    cls: "tok-kw",
  },
  {
    re: kw(
      "int int8 int16 int32 int64 uint uint8 uint16 uint32 uint64 uintptr float32 float64 " +
        "complex64 complex128 string bool byte rune error any make new len cap append copy delete panic recover"
    ),
    cls: "tok-type",
  },
  NUM,
  CALL,
  UPPER,
  IDENT,
  WS,
  NL,
  PUNCT,
];

/* ------------------------------------------------------------------ Diff */

const DIFF: Rule[] = [
  { re: /^(?:diff|index|new file|deleted file|old mode|new mode|similarity|dissimilarity|rename|copy|Binary files)[^\n]*/my, cls: "tok-com" },
  { re: /^(?:---|\+\+\+)[^\n]*/my, cls: "tok-com" },
  { re: /^@@[^\n]*/my, cls: "tok-kw" },
  { re: /^\+[^\n]*/my, cls: "tok-add" },
  { re: /^-[^\n]*/my, cls: "tok-del" },
  { re: /^[^\n]+/my, cls: null },
  NL,
];

/* -------------------------------------------------------------- 语言注册 */

const LANGS: Record<string, Rule[]> = {
  rust: RUST,
  ts: TS,
  python: PYTHON,
  json: JSON_RULES,
  bash: BASH,
  toml: TOML,
  yaml: YAML,
  html: HTML,
  css: CSS,
  sql: SQL,
  go: GO,
  diff: DIFF,
};

const ALIASES: Record<string, string> = {
  rust: "rust",
  rs: "rust",

  ts: "ts",
  tsx: "ts",
  typescript: "ts",
  js: "ts",
  jsx: "ts",
  javascript: "ts",
  mjs: "ts",
  cjs: "ts",
  node: "ts",

  py: "python",
  python: "python",
  python3: "python",

  json: "json",
  jsonc: "json",
  json5: "json",

  bash: "bash",
  sh: "bash",
  shell: "bash",
  zsh: "bash",
  console: "bash",
  "shell-session": "bash",
  shellscript: "bash",

  toml: "toml",
  cargo: "toml",

  yaml: "yaml",
  yml: "yaml",

  html: "html",
  xml: "html",
  svg: "html",
  vue: "html",
  xhtml: "html",

  css: "css",
  scss: "css",
  less: "css",

  sql: "sql",
  postgres: "sql",
  postgresql: "sql",
  mysql: "sql",
  sqlite: "sql",

  go: "go",
  golang: "go",

  diff: "diff",
  patch: "diff",
};

/** 语言别名 → 规则表键；不认识返回 null。 */
export function normalizeLang(lang: string | null | undefined): string | null {
  if (!lang) return null;
  const key = lang.trim().toLowerCase();
  return ALIASES[key] ?? null;
}

/** 该语言是否有着色规则（渲染层可据此决定是否显示语言徽标）。 */
export function isHighlightable(lang: string | null | undefined): boolean {
  return normalizeLang(lang) !== null;
}

/* -------------------------------------------------------------------- 扫描 */

function merge(tokens: Token[]): Token[] {
  const out: Token[] = [];
  for (const token of tokens) {
    if (!token.text) continue;
    const last = out.length > 0 ? out[out.length - 1] : null;
    if (last && last.cls === token.cls) last.text += token.text;
    else out.push({ text: token.text, cls: token.cls });
  }
  return out;
}

export function highlight(code: string, lang: string | null): Token[] {
  if (!code) return [];
  const key = normalizeLang(lang);
  const rules = key ? LANGS[key] : undefined;
  if (!rules || code.length > MAX_HIGHLIGHT_CHARS) {
    return [{ text: code, cls: null }];
  }

  const out: Token[] = [];
  let i = 0;
  let plain = "";

  scan: while (i < code.length) {
    for (const rule of rules) {
      rule.re.lastIndex = i;
      const m = rule.re.exec(code);
      if (m === null || m.index !== i || m[0].length === 0) continue;
      if (plain) {
        out.push({ text: plain, cls: null });
        plain = "";
      }
      out.push({ text: m[0], cls: rule.cls });
      i += m[0].length;
      continue scan;
    }
    plain += code.charAt(i);
    i++;
  }
  if (plain) out.push({ text: plain, cls: null });

  return merge(out);
}
