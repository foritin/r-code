const AUTHORED_BLOCK_MARKDOWN = /(^|\n)\s{0,3}(?:#{1,6}\s+|[-+*]\s+|\d+[.)]\s+|>\s+|```|~~~)/m;

const SECTION_LABEL_SOURCE = [
  "改造",
  "实现(?:要点)?",
  "验收(?:标准|条件|边界)?",
  "验证(?:方式)?",
  "涉及文件",
  "文件",
  "风险",
  "说明",
  "Implementation",
  "Acceptance(?:\\s+criteria)?",
  "Verification",
  "Files?",
  "Risks?",
  "Notes?",
].join("|");

interface DescriptionSection {
  label: string;
  body: string;
}

function sectionPattern(): RegExp {
  return new RegExp(
    `(?:^|[\\r\\n]+|[；;。]\\s*)(${SECTION_LABEL_SOURCE})\\s*[：:]\\s*`,
    "giu",
  );
}

function trimBoundary(value: string): string {
  return value.replace(/^[\s；;]+|[\s；;]+$/g, "").trim();
}

function numberedParts(body: string): Array<{ number: string; text: string }> | null {
  const marker = /(?:^|[；;]\s*)(\d{1,2})[.)、）]\s*/g;
  const matches = [...body.matchAll(marker)];
  if (matches.length < 2) return null;

  const prefix = trimBoundary(body.slice(0, matches[0].index ?? 0));
  if (prefix) return null;

  const parts = matches
    .map((match, index) => {
      const start = (match.index ?? 0) + match[0].length;
      const end = matches[index + 1]?.index ?? body.length;
      return {
        number: match[1],
        text: trimBoundary(body.slice(start, end)),
      };
    })
    .filter((part) => part.text.length > 0);

  return parts.length >= 2 ? parts : null;
}

function listFriendlySection(label: string): boolean {
  return /^(?:改造|实现|验收|验证|风险|implementation|acceptance|verification|risks?)/i.test(label);
}

function formatSectionBody(section: DescriptionSection): string {
  const numbered = numberedParts(section.body);
  if (numbered) {
    return numbered.map((part) => `${part.number}. ${part.text}`).join("\n");
  }

  const statements = section.body
    .split(/(?:[；;]|\r?\n)\s*/)
    .map(trimBoundary)
    .filter(Boolean);
  if (listFriendlySection(section.label) && statements.length >= 2) {
    return statements.map((statement) => `- ${statement}`).join("\n");
  }

  return section.body;
}

/**
 * Preserve authored Markdown. For legacy/provider output that flattened several named sections
 * into one long paragraph, recover only obvious labels and list separators. Short prose is left
 * untouched so the Plan does not become visually noisy merely because Markdown is supported.
 */
export function formatPlanDescriptionMarkdown(source: string): string {
  const text = source.trim();
  if (!text || AUTHORED_BLOCK_MARKDOWN.test(text) || /\n\s*\n/.test(text)) return text;

  const matches = [...text.matchAll(sectionPattern())];
  if (matches.length === 0) return text;

  const semicolonCount = (text.match(/[；;]/g) ?? []).length;
  const isStructured = text.length >= 160 || matches.length >= 2 || semicolonCount >= 2;
  if (!isStructured) return text;

  const blocks: string[] = [];
  const prefix = trimBoundary(text.slice(0, matches[0].index ?? 0));
  if (prefix) blocks.push(prefix);

  matches.forEach((match, index) => {
    const start = (match.index ?? 0) + match[0].length;
    const end = matches[index + 1]?.index ?? text.length;
    const body = trimBoundary(text.slice(start, end));
    if (!body) return;
    const section = { label: match[1].trim(), body };
    blocks.push(`### ${section.label}\n\n${formatSectionBody(section)}`);
  });

  return blocks.length > 0 ? blocks.join("\n\n") : text;
}
