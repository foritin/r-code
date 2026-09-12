/**
 * R07b — 能力 → UI 投影（纯 TS）：审批卡按钮显隐、输入框可用性、
 * 只读徽标。权威能力集来自 hello 的 welcome（daemon 强制，F6）；
 * 本投影只决定 UI 呈现，任何越权操作仍由 daemon 拒绝。
 */

export interface DeviceCapabilities {
  eventsRead: boolean;
  tasksWrite: boolean;
  approvalsDecide: boolean;
}

/** welcome.capabilities 标签（kebab-case）→ 投影。 */
export function capabilitiesFromLabels(labels: readonly string[]): DeviceCapabilities {
  return {
    eventsRead: labels.includes("events-read"),
    tasksWrite: labels.includes("tasks-write"),
    approvalsDecide: labels.includes("approvals-decide"),
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
  return [
    { label: "已授能力（在电脑端管理）", value: granted.length > 0 ? granted.join("、") : "无" },
  ];
}
