/**
 * R07b — 能力 → UI 投影（纯 TS）：审批卡按钮显隐、输入框可用性、
 * 只读徽标。权威能力集来自 hello 的 welcome（daemon 强制，F6）；
 * 本投影只决定 UI 呈现，任何越权操作仍由 daemon 拒绝。
 */

export interface DeviceCapabilities {
  eventsRead: boolean;
  tasksWrite: boolean;
  approvalsDecide: boolean;
  /**
   * P19B-C：请求/撤销 effect 授权是**独立作用域**。`approvals:decide` 只覆盖
   * 「对已存在的待决操作做决策」，它从不隐含「凭空发起授权」或「撤销已落库的
   * 授权」——后者是更强、更持久的能力，必须单独授予。
   */
  effectsManage: boolean;
}

/** welcome.capabilities 标签（kebab-case）→ 投影。 */
export function capabilitiesFromLabels(labels: readonly string[]): DeviceCapabilities {
  return {
    eventsRead: labels.includes("events-read"),
    tasksWrite: labels.includes("tasks-write"),
    approvalsDecide: labels.includes("approvals-decide"),
    effectsManage: labels.includes("effects-manage"),
  };
}

export interface ApprovalCardUi {
  /** 决策按钮可见（缺能力即隐藏——不是禁用，任务卡 R07b.A1）。 */
  showDecideButtons: boolean;
  /** 只读徽标文案；可决策时为 null。 */
  readOnlyBadge: string | null;
}

export function approvalCardUi(capabilities: DeviceCapabilities): ApprovalCardUi {
  if (capabilities.approvalsDecide) {
    return { showDecideButtons: true, readOnlyBadge: null };
  }
  return {
    showDecideButtons: false,
    readOnlyBadge: "只读设备 · 不可决策",
  };
}

export interface EffectApprovalUi {
  /** 对 effect 待决请求做决策（沿用 approvals:decide）。 */
  canDecide: boolean;
  /** 发起请求 / 撤销已落库授权（独立作用域 effects-manage）。 */
  canManage: boolean;
  canRevoke: boolean;
  readOnlyBadge: string | null;
  /** 撤销是 supersede 且仅影响未来运行——必须可见，不得静默翻状态。 */
  revokeWarning: string;
  reauthorizeHint: string;
  decisionHint: string;
}

/**
 * 效果授权控件投影。缺能力即**隐藏**（不是禁用）并给只读徽标，失败关闭：
 * 一个写请求都不发。撤销警告与重新授权提示在所有能力组合下都返回，
 * 因为它们陈述的是已冻结的语义（仅未来运行、id 终身不复用），与能力无关。
 */
export function effectApprovalUi(
  capabilities: DeviceCapabilities,
  hasActive: boolean,
): EffectApprovalUi {
  return {
    canDecide: capabilities.approvalsDecide,
    canManage: capabilities.effectsManage,
    canRevoke: capabilities.effectsManage && hasActive,
    readOnlyBadge: capabilities.effectsManage ? null : "只读设备 · 不可请求或撤销授权",
    revokeWarning: "撤销仅影响未来运行：已经冻结的运行快照不会被改写。",
    reauthorizeHint: "授权编号终身不复用；撤销后重新授权需要发起一次新的请求。",
    decisionHint: "拒绝或过期不会留下任何授权记录。",
  };
}


/** 输入框/中止按钮可用性（tasks:write）。 */
export function composerUi(capabilities: DeviceCapabilities): {
  canSend: boolean;
  canAbort: boolean;
  placeholder: string;
} {
  return {
    canSend: capabilities.tasksWrite,
    canAbort: capabilities.tasksWrite,
    placeholder: capabilities.tasksWrite ? "发送给这台电脑…" : "只读设备 · 不可发送",
  };
}

/** 设备页投影（屏④只读：能力授予在桌面端）。 */
export function deviceInfoUi(capabilities: DeviceCapabilities): Array<{ label: string; value: string }> {
  const granted: string[] = [];
  if (capabilities.eventsRead) granted.push("事件读取");
  if (capabilities.tasksWrite) granted.push("任务写入");
  if (capabilities.approvalsDecide) granted.push("审批决策");
  if (capabilities.effectsManage) granted.push("效果授权管理");
  return [
    { label: "已授能力（在电脑端管理）", value: granted.length > 0 ? granted.join("、") : "无" },
  ];
}
