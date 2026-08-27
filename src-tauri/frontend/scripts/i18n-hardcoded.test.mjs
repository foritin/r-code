import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import test from "node:test";
import { fileURLToPath } from "node:url";

import ts from "typescript";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const sourceDir = path.join(frontendDir, "src");
const baselinePath = path.join(frontendDir, "scripts", "i18n-hardcoded-baseline.json");
const userTextAttributes = new Set([
  "alt",
  "aria-description",
  "aria-label",
  "description",
  "emptyText",
  "helperText",
  "label",
  "placeholder",
  "title",
]);

function sourceFiles(directory) {
  return fs.readdirSync(directory, { withFileTypes: true })
    .flatMap((entry) => {
      const absolute = path.join(directory, entry.name);
      if (entry.isDirectory()) return sourceFiles(absolute);
      return entry.isFile() && entry.name.endsWith(".tsx") ? [absolute] : [];
    });
}

function normalizeText(value) {
  return value.replace(/\s+/g, " ").trim();
}

function isUserCopy(value) {
  const normalized = normalizeText(value);
  return normalized.length > 0 && /[\p{L}\p{Script=Han}]/u.test(normalized);
}

function isTranslationCall(node) {
  if (!ts.isCallExpression(node)) return false;
  if (ts.isIdentifier(node.expression)) {
    return node.expression.text === "t" || node.expression.text === "translate";
  }
  return ts.isPropertyAccessExpression(node.expression) && node.expression.name.text === "t";
}

function expressionStrings(node, result) {
  const visit = (child) => {
    if (isTranslationCall(child)) return;
    if (ts.isStringLiteral(child) || ts.isNoSubstitutionTemplateLiteral(child)) {
      if (isUserCopy(child.text)) result.push(normalizeText(child.text));
      return;
    }
    ts.forEachChild(child, visit);
  };
  visit(node);
  return result;
}

function hardcodedCopyFromSource(source, absolutePath = "fixture.tsx") {
  const sourceFile = ts.createSourceFile(
    absolutePath,
    source,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TSX,
  );
  const findings = [];
  const visit = (node) => {
    if (ts.isJsxText(node) && isUserCopy(node.text)) {
      findings.push(`text:${normalizeText(node.text)}`);
    } else if (ts.isJsxAttribute(node)) {
      const name = node.name.getText(sourceFile);
      if (userTextAttributes.has(name) && node.initializer) {
        if (ts.isStringLiteral(node.initializer) && isUserCopy(node.initializer.text)) {
          findings.push(`${name}:${normalizeText(node.initializer.text)}`);
        } else if (ts.isJsxExpression(node.initializer) && node.initializer.expression) {
          for (const value of expressionStrings(node.initializer.expression, [])) {
            findings.push(`${name}:${value}`);
          }
        }
      }
      return;
    } else if (
      ts.isJsxExpression(node)
      && node.expression
      && !ts.isJsxAttribute(node.parent)
    ) {
      for (const value of expressionStrings(node.expression, [])) {
        findings.push(`expression:${value}`);
      }
      return;
    }
    ts.forEachChild(node, visit);
  };
  visit(sourceFile);
  return findings.sort();
}

function hardcodedCopy(absolutePath) {
  return hardcodedCopyFromSource(fs.readFileSync(absolutePath, "utf8"), absolutePath);
}

function observedBaseline() {
  const baseline = {};
  for (const absolutePath of sourceFiles(sourceDir).sort()) {
    const findings = hardcodedCopy(absolutePath);
    if (findings.length === 0) continue;
    const relativePath = path.relative(frontendDir, absolutePath).replaceAll("\\", "/");
    baseline[relativePath] = {
      count: findings.length,
      sha256: crypto.createHash("sha256").update(JSON.stringify(findings)).digest("hex"),
    };
  }
  return baseline;
}

const observed = observedBaseline();

if (process.argv.includes("--print-baseline")) {
  process.stdout.write(`${JSON.stringify(observed, null, 2)}\n`);
} else {
  test("the guard detects JSX copy while ignoring translation catalog keys", () => {
    assert.deepEqual(
      hardcodedCopyFromSource(`
        const View = ({ t, ready }) => (
          <section aria-label={t("updater.sectionLabel")}>
            <h1>Check for updates</h1>
            <button title="Restart later">{ready ? "Install now" : t("updater.check")}</button>
          </section>
        );
      `),
      [
        "expression:Install now",
        "text:Check for updates",
        "title:Restart later",
      ],
    );
  });

  test("JSX user copy cannot grow outside the reviewed i18n migration baseline", () => {
    const expected = JSON.parse(fs.readFileSync(baselinePath, "utf8"));
    assert.deepEqual(
      observed,
      expected,
      "Hardcoded JSX copy changed. New copy must use i18n keys; intentional legacy removal updates the reviewed baseline.",
    );
  });
}
