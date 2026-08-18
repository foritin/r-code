/**
 * 文件类型图标 —— 活动流文件行的扩展名识别。
 *
 * 图标资产位于 src/assets/filetypes/，来自两个 MIT 图标集：
 * vscode-icons（经 Iconify 分发）提供 logo 类图标；markdown/toml/license/
 * rust/zip/bat/svg 这类通用图形图标改用 Material Icon Theme
 * （material-extensions/vscode-material-icon-theme）——实心填充在暗色
 * 主题 15px 下远比 vscode-icons 的描边/深色款清晰。c.svg 是上游深色版
 * 本地提亮。未覆盖的扩展回退到按语义分色的通用文档形，
 * 颜色沿用 Signature 暖色深浅底下仍可分辨的中低饱和色。
 */
import cIcon from "../../assets/filetypes/c.svg";
import batIcon from "../../assets/filetypes/bat.svg";
import configIcon from "../../assets/filetypes/config.svg";
import cppIcon from "../../assets/filetypes/cpp.svg";
import cheaderIcon from "../../assets/filetypes/cheader.svg";
import csharpIcon from "../../assets/filetypes/csharp.svg";
import cssIcon from "../../assets/filetypes/css.svg";
import dartIcon from "../../assets/filetypes/dart.svg";
import dockerIcon from "../../assets/filetypes/docker.svg";
import editorconfigIcon from "../../assets/filetypes/editorconfig.svg";
import excelIcon from "../../assets/filetypes/excel.svg";
import gitIcon from "../../assets/filetypes/git.svg";
import goIcon from "../../assets/filetypes/go.svg";
import htmlIcon from "../../assets/filetypes/html.svg";
import imageIcon from "../../assets/filetypes/image.svg";
import javaIcon from "../../assets/filetypes/java.svg";
import jsIcon from "../../assets/filetypes/js.svg";
import jsonIcon from "../../assets/filetypes/json.svg";
import jsxIcon from "../../assets/filetypes/jsx.svg";
import kotlinIcon from "../../assets/filetypes/kotlin.svg";
import lessIcon from "../../assets/filetypes/less.svg";
import licenseIcon from "../../assets/filetypes/license.svg";
import markdownIcon from "../../assets/filetypes/markdown.svg";
import pdfIcon from "../../assets/filetypes/pdf.svg";
import phpIcon from "../../assets/filetypes/php.svg";
import powershellIcon from "../../assets/filetypes/powershell.svg";
import pythonIcon from "../../assets/filetypes/python.svg";
import rubyIcon from "../../assets/filetypes/ruby.svg";
import rustIcon from "../../assets/filetypes/rust.svg";
import sassIcon from "../../assets/filetypes/scss.svg";
import shellIcon from "../../assets/filetypes/shell.svg";
import sqlIcon from "../../assets/filetypes/sql.svg";
import svgIcon from "../../assets/filetypes/svg.svg";
import swiftIcon from "../../assets/filetypes/swift.svg";
import textIcon from "../../assets/filetypes/text.svg";
import tomlIcon from "../../assets/filetypes/toml.svg";
import tsIcon from "../../assets/filetypes/typescript.svg";
import tsxIcon from "../../assets/filetypes/tsx.svg";
import vueIcon from "../../assets/filetypes/vue.svg";
import wordIcon from "../../assets/filetypes/word.svg";
import xmlIcon from "../../assets/filetypes/xml.svg";
import yamlIcon from "../../assets/filetypes/yaml.svg";
import zipIcon from "../../assets/filetypes/zip.svg";

const EXTENSION_ICONS: Record<string, string> = {
  ts: tsIcon, tsx: tsxIcon,
  js: jsIcon, mjs: jsIcon, cjs: jsIcon, jsx: jsxIcon,
  rs: rustIcon,
  css: cssIcon, scss: sassIcon, sass: sassIcon, less: lessIcon,
  html: htmlIcon, htm: htmlIcon, svelte: htmlIcon,
  vue: vueIcon,
  md: markdownIcon, mdx: markdownIcon,
  json: jsonIcon, toml: tomlIcon, yaml: yamlIcon, yml: yamlIcon,
  ini: configIcon, cfg: configIcon, conf: configIcon, env: configIcon, properties: configIcon, lock: configIcon, editorconfig: editorconfigIcon,
  png: imageIcon, jpg: imageIcon, jpeg: imageIcon, gif: imageIcon, webp: imageIcon, bmp: imageIcon, ico: imageIcon,
  svg: svgIcon,
  txt: textIcon, log: textIcon,
  py: pythonIcon, pyw: pythonIcon,
  go: goIcon, java: javaIcon, kt: kotlinIcon, kts: kotlinIcon,
  c: cIcon, cpp: cppIcon, cc: cppIcon, cxx: cppIcon, "c++": cppIcon,
  h: cheaderIcon, hpp: cheaderIcon,
  cs: csharpIcon, php: phpIcon, rb: rubyIcon, swift: swiftIcon, dart: dartIcon,
  sh: shellIcon, bash: shellIcon, zsh: shellIcon, fish: shellIcon,
  ps1: powershellIcon,
  bat: batIcon, cmd: batIcon,
  sql: sqlIcon, xml: xmlIcon, xsl: xmlIcon,
  pdf: pdfIcon,
  zip: zipIcon, tar: zipIcon, gz: zipIcon, "7z": zipIcon, rar: zipIcon,
  doc: wordIcon, docx: wordIcon, xls: excelIcon, xlsx: excelIcon, csv: excelIcon,
};

/** 特殊文件名（无扩展名或语义优先于扩展名）优先按名字匹配。 */
const BASENAME_ICONS: Record<string, string> = {
  dockerfile: dockerIcon,
  dockerignore: dockerIcon,
  "compose.yaml": dockerIcon,
  "compose.yml": dockerIcon,
  license: licenseIcon,
  licence: licenseIcon,
  ".gitignore": gitIcon,
  ".gitattributes": gitIcon,
  ".gitmodules": gitIcon,
  ".editorconfig": editorconfigIcon,
  ".npmrc": configIcon,
  ".env": configIcon,
};

const EXTENSION_TONES: Record<string, string> = {
  ts: "#7fa7e0", tsx: "#7fa7e0", js: "#7fa7e0", jsx: "#7fa7e0", mjs: "#7fa7e0", cjs: "#7fa7e0",
  rs: "#dea584",
  css: "#b48ae0", scss: "#b48ae0", less: "#b48ae0",
  html: "#e08a5a", htm: "#e08a5a", svelte: "#e08a5a",
  vue: "#6fbf8f",
  md: "#c49a61", mdx: "#c49a61",
  txt: "#a49f95",
  json: "#d8b56b", toml: "#d8b56b", yaml: "#d8b56b", yml: "#d8b56b", ini: "#d8b56b",
  lock: "#817b72",
  png: "#6fbf8f", jpg: "#6fbf8f", jpeg: "#6fbf8f", gif: "#6fbf8f", svg: "#6fbf8f", webp: "#6fbf8f", ico: "#6fbf8f",
  py: "#7fbf9e",
};

function fileBase(path: string): string {
  const parts = path.split(/[\\/]/);
  return (parts[parts.length - 1] ?? path).toLowerCase();
}

function fileExtension(path: string): string {
  const base = fileBase(path);
  const dot = base.lastIndexOf(".");
  return dot > 0 ? base.slice(dot + 1) : "";
}

export function fileIconUrl(path: string): string | null {
  const base = fileBase(path);
  const byName = BASENAME_ICONS[base];
  if (byName) return byName;
  const byExt = EXTENSION_ICONS[fileExtension(path)];
  if (byExt) return byExt;
  // 点文件（.gitignore、.env 等）按去掉前导点后的名字再匹配一次。
  if (base.startsWith(".")) return BASENAME_ICONS[base.slice(1)] ?? null;
  return null;
}

export function fileTone(path: string): string {
  return EXTENSION_TONES[fileExtension(path)] ?? "currentColor";
}

export function FileTypeIcon({ path, size = 15 }: { path: string; size?: number }) {
  const url = fileIconUrl(path);
  if (url) {
    return <img src={url} width={size} height={size} alt="" aria-hidden="true" draggable={false} className="file-type-icon-img" />;
  }
  const tone = fileTone(path);
  return (
    <svg viewBox="0 0 24 24" width={size} height={size} aria-hidden="true">
      <path d="M6 2.5h7.5L18 7v14.5a1 1 0 0 1-1 1H7a1 1 0 0 1-1-1Z" fill={tone} />
      <path d="M13.5 2.5V7H18Z" fill="rgba(0,0,0,0.28)" />
    </svg>
  );
}
