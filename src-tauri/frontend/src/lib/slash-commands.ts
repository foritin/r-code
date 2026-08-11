import type { WorkflowSkill } from "./types";

export type SlashCommandLocation = "home" | "room";
export type SlashCommandCategory = "session" | "view" | "workflow" | "integration";
export type SlashCommandKind = "local" | "workflow";

export interface SlashCommandDefinition {
  name: string;
  aliases?: string[];
  title: string;
  description: string;
  category: SlashCommandCategory;
  kind: SlashCommandKind;
  locations: SlashCommandLocation[];
  argumentHint?: string;
  requiresWorkspace?: boolean;
  blockedWhileRunning?: boolean;
  requiresRunning?: boolean;
  keywords?: string[];
  skill?: WorkflowSkill;
}

export interface SlashCommandContext {
  location: SlashCommandLocation;
  workspaceAttached: boolean;
  running: boolean;
}

export interface ParsedSlashCommand {
  rawName: string;
  args: string;
  command: SlashCommandDefinition | null;
}

/**
 * R-Code 的命令面保持“少而真”：local 命令直接操作产品状态；workflow 命令会展开成
 * 一段稳定的工作流要求后交给 Agent。不会把尚未实现的能力伪装成可用按钮。
 */
export const SLASH_COMMANDS: SlashCommandDefinition[] = [
  {
    name: "clear",
    aliases: ["new", "reset"],
    title: "清空当前上下文",
    description: "当前任务切换到空白消息上下文；历史分支仍会保留用于审计。",
    category: "session",
    kind: "local",
    locations: ["home", "room"],
    blockedWhileRunning: true,
    keywords: ["清空", "上下文", "会话"],
  },
  {
    name: "resume",
    aliases: ["tasks", "history"],
    title: "恢复历史会话",
    description: "打开会话列表，继续先前任务或查看已归档记录。",
    category: "session",
    kind: "local",
    locations: ["home", "room"],
    keywords: ["继续", "历史", "任务"],
  },
  {
    name: "compact",
    title: "压缩上下文",
    description: "总结较早内容，保留最近对话并继续当前会话。",
    category: "session",
    kind: "local",
    locations: ["room"],
    argumentHint: "[希望摘要重点保留的内容]",
    blockedWhileRunning: true,
    keywords: ["摘要", "上下文", "token"],
  },
  {
    name: "fork",
    title: "分支当前会话",
    description: "保留当前分支，从同一上下文末端创建新的可继续分支。",
    category: "session",
    kind: "local",
    locations: ["room"],
    blockedWhileRunning: true,
    keywords: ["分支", "副本", "试验"],
  },
  {
    name: "rename",
    title: "重命名会话",
    description: "修改左侧会话列表中显示的名称。",
    category: "session",
    kind: "local",
    locations: ["room"],
    argumentHint: "<新名称>",
    blockedWhileRunning: true,
    keywords: ["标题", "名称"],
  },
  {
    name: "context",
    aliases: ["status"],
    title: "查看当前上下文",
    description: "显示会话、模型、权限、消息与运行状态。",
    category: "session",
    kind: "local",
    locations: ["home", "room"],
    keywords: ["状态", "模型", "用量"],
  },
  {
    name: "usage",
    title: "估算上下文用量",
    description: "显示当前可见消息量与粗略 token 估算，不冒充服务商账单。",
    category: "session",
    kind: "local",
    locations: ["room"],
    keywords: ["token", "字符", "额度"],
  },
  {
    name: "copy",
    title: "复制最近回复",
    description: "复制当前会话中最近一条 Agent 回复。",
    category: "session",
    kind: "local",
    locations: ["room"],
    keywords: ["剪贴板", "回复"],
  },
  {
    name: "export",
    title: "复制会话 Markdown",
    description: "把当前可见对话整理成 Markdown 并复制到剪贴板。",
    category: "session",
    kind: "local",
    locations: ["room"],
    keywords: ["导出", "markdown", "记录"],
  },
  {
    name: "stop",
    title: "停止当前运行",
    description: "停止主 Agent，并级联停止仍在运行的子代理。",
    category: "session",
    kind: "local",
    locations: ["room"],
    requiresRunning: true,
    keywords: ["中断", "取消"],
  },
  {
    name: "model",
    title: "选择模型",
    description: "打开当前会话的模型与服务选择器。",
    category: "view",
    kind: "local",
    locations: ["home", "room"],
    blockedWhileRunning: true,
    keywords: ["provider", "服务"],
  },
  {
    name: "search",
    title: "全局搜索",
    description: "搜索任务、项目文件和历史对话。",
    category: "view",
    kind: "local",
    locations: ["home", "room"],
    keywords: ["查找", "文件", "对话"],
  },
  {
    name: "pending",
    aliases: ["inbox"],
    title: "查看待处理",
    description: "打开权限请求、失败运行和需要人工处理的事项。",
    category: "view",
    kind: "local",
    locations: ["home", "room"],
    keywords: ["审批", "收件箱", "失败"],
  },
  {
    name: "activity",
    title: "查看全局活动",
    description: "打开跨项目任务动态；项目内动态仍只在项目页面显示。",
    category: "view",
    kind: "local",
    locations: ["home", "room"],
    keywords: ["动态", "运行", "审计"],
  },
  {
    name: "projects",
    aliases: ["workspaces"],
    title: "添加或打开项目",
    description: "从本地添加项目，或进入已有项目工作台。",
    category: "view",
    kind: "local",
    locations: ["home", "room"],
    keywords: ["工作区", "文件夹", "记忆"],
  },
  {
    name: "permissions",
    aliases: ["permission"],
    title: "调整项目权限",
    description: "打开当前项目的批准策略。",
    category: "view",
    kind: "local",
    locations: ["home", "room"],
    requiresWorkspace: true,
    blockedWhileRunning: true,
    keywords: ["访问", "审批", "权限"],
  },
  {
    name: "agents",
    aliases: ["subagents", "agent"],
    title: "展开子代理",
    description: "展开运行树；点击子代理可查看公开进度与结果。",
    category: "view",
    kind: "local",
    locations: ["room"],
    keywords: ["子代理", "并行"],
  },
  {
    name: "diff",
    aliases: ["changes"],
    title: "查看变更",
    description: "打开当前任务的文件变更与差异视图。",
    category: "view",
    kind: "local",
    locations: ["room"],
    requiresWorkspace: true,
    keywords: ["改动", "文件"],
  },
  {
    name: "undo",
    aliases: ["rewind"],
    title: "检查并撤销变更",
    description: "打开变更页；逐文件或整任务回滚仍需再次确认。",
    category: "view",
    kind: "local",
    locations: ["room"],
    requiresWorkspace: true,
    blockedWhileRunning: true,
    keywords: ["回滚", "撤销", "恢复"],
  },
  {
    name: "files",
    title: "打开项目文件",
    description: "切换到当前项目的文件浏览与轻量编辑视图。",
    category: "view",
    kind: "local",
    locations: ["room"],
    requiresWorkspace: true,
    keywords: ["文件", "编辑器"],
  },
  {
    name: "terminal",
    title: "打开终端",
    description: "切换到当前项目的跨平台终端。",
    category: "view",
    kind: "local",
    locations: ["room"],
    requiresWorkspace: true,
    keywords: ["shell", "命令行"],
  },
  {
    name: "review",
    title: "打开审阅",
    description: "无参数时打开审阅页；带参数时启动代码审查工作流。",
    category: "view",
    kind: "local",
    locations: ["room"],
    argumentHint: "[审查范围]",
    requiresWorkspace: true,
    keywords: ["审查", "审核", "验证"],
  },
  {
    name: "verify",
    aliases: ["test", "run"],
    title: "运行验证",
    description: "带命令时直接运行验证；无参数时打开审阅页。",
    category: "view",
    kind: "local",
    locations: ["room"],
    argumentHint: "[cargo test / npm test / …]",
    requiresWorkspace: true,
    keywords: ["测试", "构建", "检查"],
  },
  {
    name: "memory",
    aliases: ["knowledge", "instructions"],
    title: "知识与指令",
    description: "管理全局/项目记忆、协作 Prompt 与 Skills。",
    category: "view",
    kind: "local",
    locations: ["home", "room"],
    keywords: ["约定", "上下文", "偏好", "prompt", "skill"],
  },
  {
    name: "theme",
    title: "切换外观",
    description: "切换亮色、暗色或跟随系统。",
    category: "view",
    kind: "local",
    locations: ["home", "room"],
    argumentHint: "[light | dark | system]",
    keywords: ["亮色", "暗色", "主题"],
  },
  {
    name: "settings",
    title: "打开设置",
    description: "管理模型服务、外观、诊断与 Codex CLI。",
    category: "view",
    kind: "local",
    locations: ["home", "room"],
    keywords: ["配置"],
  },
  {
    name: "plan",
    title: "先制定计划",
    description: "先澄清目标与风险，只输出可执行计划，不修改文件。",
    category: "workflow",
    kind: "workflow",
    locations: ["home", "room"],
    argumentHint: "[目标]",
    keywords: ["规划", "方案"],
  },
  {
    name: "doctor",
    title: "项目体检",
    description: "只读检查结构、配置、依赖、测试与明显风险。",
    category: "workflow",
    kind: "workflow",
    locations: ["home", "room"],
    argumentHint: "[关注点]",
    requiresWorkspace: true,
    keywords: ["诊断", "健康", "质量"],
  },
  {
    name: "debug",
    title: "系统化排障",
    description: "从复现与证据入手定位根因；默认不直接改代码。",
    category: "workflow",
    kind: "workflow",
    locations: ["home", "room"],
    argumentHint: "<故障现象>",
    requiresWorkspace: true,
    keywords: ["调试", "根因", "bug"],
  },
  {
    name: "fix",
    title: "诊断并修复",
    description: "复现问题、定位根因、实施最小修复并完成相关验证。",
    category: "workflow",
    kind: "workflow",
    locations: ["home", "room"],
    argumentHint: "<问题或失败现象>",
    requiresWorkspace: true,
    keywords: ["修复", "bug", "实现"],
  },
  {
    name: "explain",
    title: "解释代码或方案",
    description: "结合项目上下文说明行为、边界和关键取舍。",
    category: "workflow",
    kind: "workflow",
    locations: ["home", "room"],
    argumentHint: "<文件、符号或问题>",
    keywords: ["说明", "理解"],
  },
  {
    name: "init",
    title: "初始化项目指引",
    description: "检查仓库后创建或完善 AGENTS.md 项目说明。",
    category: "workflow",
    kind: "workflow",
    locations: ["home", "room"],
    requiresWorkspace: true,
    keywords: ["AGENTS.md", "项目规范"],
  },
  {
    name: "code-review",
    title: "代码审查",
    description: "只读审查正确性、回归风险、测试缺口与可维护性。",
    category: "workflow",
    kind: "workflow",
    locations: ["home", "room"],
    argumentHint: "[范围]",
    requiresWorkspace: true,
    keywords: ["review", "质量"],
  },
  {
    name: "security-review",
    title: "安全审查",
    description: "只读检查权限边界、输入处理、凭据和供应链风险。",
    category: "workflow",
    kind: "workflow",
    locations: ["home", "room"],
    argumentHint: "[范围]",
    requiresWorkspace: true,
    keywords: ["安全", "漏洞", "权限"],
  },
  {
    name: "simplify",
    aliases: ["refactor"],
    title: "简化实现",
    description: "在保持行为与测试的前提下减少复杂度和重复。",
    category: "workflow",
    kind: "workflow",
    locations: ["home", "room"],
    argumentHint: "[范围]",
    requiresWorkspace: true,
    keywords: ["重构", "精简"],
  },
  {
    name: "docs",
    aliases: ["document"],
    title: "补全文档",
    description: "根据当前实现补齐面向维护者的准确文档。",
    category: "workflow",
    kind: "workflow",
    locations: ["home", "room"],
    argumentHint: "[范围]",
    requiresWorkspace: true,
    keywords: ["文档", "README"],
  },
  {
    name: "research",
    title: "深度调研",
    description: "先收集项目与权威资料证据，再给出带依据的结论。",
    category: "workflow",
    kind: "workflow",
    locations: ["home", "room"],
    argumentHint: "<调研问题>",
    keywords: ["调查", "资料", "对比"],
  },
  {
    name: "qa",
    title: "质量验证",
    description: "运行与改动相关的构建、测试和静态检查并处理失败。",
    category: "workflow",
    kind: "workflow",
    locations: ["home", "room"],
    argumentHint: "[验证范围]",
    requiresWorkspace: true,
    keywords: ["测试", "构建", "回归"],
  },
  {
    name: "mcp",
    title: "联网与 MCP",
    description: "打开“设置 → 工具与连接”，管理联网工具、MCP 服务与市场。",
    category: "integration",
    kind: "local",
    locations: ["home", "room"],
    keywords: ["工具", "服务"],
  },
  {
    name: "skills",
    title: "查看内置工作流",
    description: "列出可直接调用的 R-Code 工作流命令。",
    category: "integration",
    kind: "local",
    locations: ["home", "room"],
    keywords: ["技能", "工作流"],
  },
  {
    name: "plugins",
    title: "扩展与插件",
    description: "查看 R-Code 的 Skill、MCP 与 Codex 扩展入口。",
    category: "integration",
    kind: "local",
    locations: ["home", "room"],
    keywords: ["扩展", "插件"],
  },
  {
    name: "help",
    aliases: ["commands"],
    title: "命令帮助",
    description: "查看命令分类、别名和使用方式。",
    category: "integration",
    kind: "local",
    locations: ["home", "room"],
    keywords: ["帮助", "命令"],
  },
];

const commandByName = new Map<string, SlashCommandDefinition>();
for (const command of SLASH_COMMANDS) {
  commandByName.set(command.name, command);
  for (const alias of command.aliases ?? []) commandByName.set(alias, command);
}

function skillCommands(skills: readonly WorkflowSkill[]): SlashCommandDefinition[] {
  return skills
    .filter((skill) => skill.enabled && !commandByName.has(skill.name))
    .map((skill) => ({
      name: skill.name,
      title: skill.name,
      description: skill.description,
      category: "workflow" as const,
      kind: "workflow" as const,
      locations: ["home", "room"] as SlashCommandLocation[],
      argumentHint: "[补充要求]",
      keywords: [skill.source === "builtin" ? "内置 skill" : "自定义 skill", "skill", "技能"],
      skill,
    }));
}

function commandLookup(skills: readonly WorkflowSkill[]): Map<string, SlashCommandDefinition> {
  const lookup = new Map(commandByName);
  for (const command of skillCommands(skills)) lookup.set(command.name, command);
  return lookup;
}

export function parseSlashCommand(
  value: string,
  skills: readonly WorkflowSkill[] = []
): ParsedSlashCommand | null {
  const match = /^\/([a-z0-9-]+)(?:\s+([\s\S]*))?\s*$/i.exec(value.trim());
  if (!match) return null;
  const rawName = match[1].toLowerCase();
  return {
    rawName,
    args: (match[2] ?? "").trim(),
    command: commandLookup(skills).get(rawName) ?? null,
  };
}

/** 只在一行开头输入命令名时显示候选；开始输入参数后让菜单退场。 */
export function slashSearchQuery(value: string): string | null {
  const match = /^\/([^\s\n]*)$/.exec(value);
  return match ? match[1].toLowerCase() : null;
}

export function commandUnavailableReason(
  command: SlashCommandDefinition,
  context: SlashCommandContext
): string | null {
  if (!command.locations.includes(context.location)) return "当前页面不可用";
  if (command.requiresWorkspace && !context.workspaceAttached) return "先附加一个项目文件夹";
  if (command.blockedWhileRunning && context.running) return "当前运行结束后可用";
  if (command.requiresRunning && !context.running) return "仅在 Agent 运行中可用";
  return null;
}

export function matchingSlashCommands(
  value: string,
  context: SlashCommandContext,
  skills: readonly WorkflowSkill[] = []
): SlashCommandDefinition[] {
  const query = slashSearchQuery(value);
  if (query == null) return [];
  const dynamicSkills = skillCommands(skills);
  // A bare slash is the discovery entry point for user workflows, so keep enabled Skills in
  // the first visible rows. Static commands remain available immediately below them.
  const catalog = query ? [...SLASH_COMMANDS, ...dynamicSkills] : [...dynamicSkills, ...SLASH_COMMANDS];
  return catalog
    .filter((command) => command.locations.includes(context.location))
    .filter((command) => {
      if (!query) return true;
      const haystack = [
        command.name,
        ...(command.aliases ?? []),
        command.title,
        command.description,
        ...(command.keywords ?? []),
      ]
        .join(" ")
        .toLowerCase();
      return haystack.includes(query);
    });
}

export function slashCommandInsertion(command: SlashCommandDefinition): string {
  return `/${command.name}${command.argumentHint ? " " : ""}`;
}

export function workflowPrompt(command: SlashCommandDefinition, args: string): string {
  const scope = args.trim();
  if (command.skill) {
    const metadata = JSON.stringify({
      id: command.skill.id,
      name: command.skill.name,
      source: command.skill.source,
      args: scope,
    });
    const supplement = scope
      ? `\n\n本次用户补充要求：\n${scope}`
      : "";
    return `[R-CODE-SKILL] ${metadata}\n\n${command.skill.instructions}${supplement}`;
  }
  let instruction: string;
  switch (command.name) {
    case "plan":
      instruction = `先不要修改文件。${scope ? `围绕“${scope}”` : "围绕当前目标"}梳理已知信息、需要确认的假设、风险、实施步骤和验证方式，给出一份可执行计划。`;
      break;
    case "doctor":
      instruction = `对当前项目做一次只读体检${scope ? `，重点检查：${scope}` : ""}。检查结构、构建配置、依赖、测试、错误处理和明显维护风险；按严重度给出带文件依据的结论，不要修改文件。`;
      break;
    case "debug":
      instruction = `系统化诊断这个问题：${scope || "当前描述的问题"}。先收集证据和稳定复现，再定位根因与影响范围；本轮只报告诊断结果，不要直接修改文件。`;
      break;
    case "fix":
      instruction = `修复这个问题：${scope || "当前描述的问题"}。先稳定复现并定位根因，再实施范围最小、兼容现有设计的修复；补充或运行相关验证，最后说明根因、改动和验证结果。`;
      break;
    case "explain":
      instruction = `结合当前上下文解释${scope ? `“${scope}”` : "当前实现"}：说明执行链路、关键状态、边界条件、失败方式和重要设计取舍。`;
      break;
    case "init":
      instruction = "检查当前仓库的技术栈、目录、构建与测试入口，然后创建或完善根目录 AGENTS.md。内容只写对后续编码代理真正有帮助、且能从仓库验证的约定；保留已有人工说明。";
      break;
    case "code-review":
      instruction = `只读审查${scope ? `以下范围：${scope}` : "当前工作区变更"}。优先查找正确性问题、回归风险、并发或状态错误、测试缺口和不可维护实现；只报告有证据的问题，按严重度排序，不要修改文件。`;
      break;
    case "security-review":
      instruction = `对${scope || "当前工作区变更"}做只读安全审查。检查权限边界、路径与输入验证、命令执行、凭据泄露、网络请求、依赖与供应链风险；给出可验证的发现和修复建议，不要修改文件。`;
      break;
    case "simplify":
      instruction = `在不改变外部行为的前提下简化${scope || "当前实现"}。先识别重复、无效抽象和不必要状态，再实施最小改动；保留或补充验证，最后说明删掉了哪些复杂度。`;
      break;
    case "docs":
      instruction = `根据当前真实实现补齐${scope ? `“${scope}”相关` : "缺失的"}维护文档。先核对代码与现有文档，避免写推测或过期说明；保持文档简洁、可执行，并验证其中的命令和路径。`;
      break;
    case "research":
      instruction = `调研这个问题：${scope || "当前目标"}。优先检查当前项目和已有资料；需要外部信息时只使用权威、可追溯来源。区分事实与推断，比较可选方案并给出带依据的结论；本轮不要修改文件。`;
      break;
    case "qa":
      instruction = `对${scope || "当前工作区变更"}做完整质量验证。先识别相关构建、测试、静态检查和高风险交互，再运行最小充分的验证；如果发现由当前改动造成的问题，修复后重新验证，最后列出通过项与剩余风险。`;
      break;
    default:
      instruction = scope;
  }
  const metadata = JSON.stringify({ name: command.name, args: scope });
  return `[R-CODE-WORKFLOW] ${metadata}\n\n${instruction}`;
}

export function parseWorkflowInvocation(value: string): { name: string; args: string } | null {
  const firstLine = value.split("\n", 1)[0];
  const prefix = "[R-CODE-WORKFLOW] ";
  if (!firstLine.startsWith(prefix)) return null;
  try {
    const parsed = JSON.parse(firstLine.slice(prefix.length)) as { name?: unknown; args?: unknown };
    if (typeof parsed.name !== "string") return null;
    return {
      name: parsed.name,
      args: typeof parsed.args === "string" ? parsed.args : "",
    };
  } catch {
    return null;
  }
}

export const CATEGORY_LABELS: Record<SlashCommandCategory, string> = {
  session: "会话",
  view: "视图与控制",
  workflow: "内置工作流",
  integration: "扩展",
};
