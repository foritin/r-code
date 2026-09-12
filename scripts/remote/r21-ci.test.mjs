/**
 * R21.A2 — mobile CI 的可证伪守卫。
 *
 * 之前这个文件只做"文件里有没有 android:/assembleDebug 字样"的静态匹配，
 * 因此 `//` 注释导致 workflow 无法解析、两个 job 是 `echo ... && exit 0`
 * 占位时它照样通过——断言没有证伪能力，等于虚假映射。
 *
 * 现在改为：workflow 必须是合法 YAML 形状、不得出现占位命令、
 * 每个 job 必须有真实可执行步骤，且必须有非空测试集（防 --passWithNoTests
 * 把零测试判成通过）。
 */
import test from "node:test";
import assert from "node:assert/strict";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..");
const workflowPath = join(root, ".github", "workflows", "mobile.yml");
const mobileDir = join(root, "mobile");

const workflowRaw = readFileSync(workflowPath, "utf8");

/** 断言前先剥离注释：注释里出现"反面示例"（如说明历史错误的字样）
 * 不应把守卫打成红灯。 */
function stripYamlComments(text) {
  return text
    .split(/\r?\n/)
    .map((line) => line.replace(/(^|\s)#.*$/, ""))
    .join("\n");
}

function stripTsComments(text) {
  return text
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .split(/\r?\n/)
    .map((line) => line.replace(/^\s*\/\/.*$/, "").replace(/\s\/\/\s.*$/, ""))
    .join("\n");
}

const workflow = workflowRaw;

test("R21.A2: mobile workflow 是合法 YAML 形状（注释必须是 #，不能是 //）", () => {
  const lines = workflow.split(/\r?\n/);
  const jsStyleComments = lines.filter(
    (line) => /^\s*\/\//.test(line) && line.trim() !== "",
  );
  assert.deepEqual(
    jsStyleComments,
    [],
    `YAML 注释只能是 #，发现 JS 风格注释行：${jsStyleComments.join(" | ")}`,
  );
  // 顶层结构齐备。
  for (const key of ["name:", "on:", "jobs:"]) {
    assert.ok(
      lines.some((line) => line.startsWith(key)),
      `缺少顶层键 ${key}`,
    );
  }
  // 每个 job 名后必须跟 runs-on（否则是形状错误）。
  const jobNames = lines
    .filter((line) => /^  [a-z][a-z0-9-]*:\s*$/.test(line))
    .map((line) => line.trim().replace(":", ""));
  assert.ok(jobNames.length >= 3, `至少 3 个 job，实际 ${jobNames.join(",")}`);
  for (const job of jobNames) {
    assert.ok(
      new RegExp(`^  ${job}:[\\s\\S]*?runs-on:`, "m").test(workflow),
      `job ${job} 缺少 runs-on`,
    );
  }
});

test("R21.A2: 不存在占位命令（echo...exit 0 之类的空转步骤）", () => {
  // 真正的构建/测试步骤不允许靠 `exit 0` 无条件成功。
  const code = stripYamlComments(workflow);
  const placeholder = code.match(/^.*&&\s*exit 0.*$/gm) ?? [];
  assert.deepEqual(placeholder, [], `发现占位命令：${placeholder.join(" | ")}`);
  const echoOnly = code.match(/^\s*run:\s*echo\s.*$/gm) ?? [];
  assert.deepEqual(echoOnly, [], `发现 echo 空转步骤：${echoOnly.join(" | ")}`);
});

test("R21.A2: 两平台各自有真实 bundle 门禁，且脚本存在", () => {
  assert.match(workflow, /macos-14/, "iOS 侧使用 macOS runner");
  assert.match(workflow, /bundle\.mjs android/, "Android bundle 门禁");
  assert.match(workflow, /bundle\.mjs ios/, "iOS bundle 门禁");
  assert.match(workflow, /tsc --noEmit/, "类型检查门禁");
  assert.match(workflow, /if-no-files-found:\s*error/, "产物缺失必须报错");
  assert.ok(existsSync(join(mobileDir, "scripts", "bundle.mjs")), "bundle 脚本存在");
  assert.ok(
    existsSync(join(mobileDir, "scripts", "bundle.mjs")) &&
      readFileSync(join(mobileDir, "scripts", "bundle.mjs"), "utf8").includes(
        "bytes < 50_000",
      ),
    "bundle 脚本必须对空/过小产物判失败（否则打包失败也会通过）",
  );
});

test("R21.A2: jest 门禁不用 --passWithNoTests 且测试集非空", () => {
  assert.doesNotMatch(
    stripYamlComments(workflow),
    /passWithNoTests/,
    "零测试必须失败，不能靠 --passWithNoTests 冒充通过",
  );
  const testsDir = join(mobileDir, "__tests__");
  assert.ok(existsSync(testsDir), "mobile/__tests__ 存在");
  const testFiles = readdirSync(testsDir).filter((name) => /\.test\.[jt]sx?$/.test(name));
  assert.ok(testFiles.length > 0, `__tests__ 内必须有测试文件，实际 ${testFiles.length} 个`);
});

test("R21.A2: RN 骨架齐备且 adapter 走平台注入（不为 RN 复制协议）", () => {
  for (const file of [
    "package.json",
    "metro.config.js",
    "babel.config.js",
    "jest.config.js",
    "tsconfig.json",
    "App.tsx",
    join("src", "adapter", "index.ts"),
    join("src", "adapter", "rn-socket.ts"),
    join("src", "screens", "SettingsScreen.tsx"),
  ]) {
    assert.ok(existsSync(join(mobileDir, file)), `mobile/${file} 存在`);
  }
  const adapter = stripTsComments(
    readFileSync(join(mobileDir, "src", "adapter", "index.ts"), "utf8"),
  );
  assert.match(adapter, /rnSocketFactory/, "RN 注入自己的 socket 工厂");
  assert.match(
    adapter,
    /clientId: config\.deviceId/,
    "身份用设备 id（F6，不能是写死的 pwa）",
  );
  assert.doesNotMatch(adapter, /addEventListener/, "RN 不用 DOM 事件 API");
});
