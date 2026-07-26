import { useEffect, useRef, useState } from "react";
import type { ProjectAccessMode } from "../lib/types";
import { IconCheck, IconChevronDown } from "./icons";

const OPTIONS: ReadonlyArray<{
  value: ProjectAccessMode;
  label: string;
  description: string;
}> = [
  {
    value: "request_approval",
    label: "请求批准",
    description: "访问外部服务、执行命令或修改文件时总是询问。",
  },
  {
    value: "risk_based",
    label: "替我审批",
    description: "仅在检测到中高风险操作时请求批准。",
  },
  {
    value: "full_access",
    label: "完全访问权限",
    description: "自动批准工作区内的低、中、高风险操作。",
  },
];

export function projectAccessModeLabel(mode: ProjectAccessMode): string {
  return OPTIONS.find((option) => option.value === mode)?.label ?? "请求批准";
}

export function projectAccessModeShortLabel(mode: ProjectAccessMode): string {
  return (
    {
      request_approval: "批准",
      risk_based: "风险",
      full_access: "完全",
    } as Record<ProjectAccessMode, string>
  )[mode];
}

interface Props {
  value: ProjectAccessMode;
  workspaceName: string;
  disabled?: boolean;
  onChange: (next: ProjectAccessMode) => Promise<void> | void;
}

/**
 * 项目级权限入口。模式只影响 Agent 的自动工具调用；本地路径始终受当前工作区边界限制。
 */
export function ProjectAccessSelector({ value, workspaceName, disabled = false, onChange }: Props) {
  const rootRef = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    const onPointerDown = (event: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onPointerDown);
    return () => document.removeEventListener("mousedown", onPointerDown);
  }, []);

  const choose = async (next: ProjectAccessMode) => {
    if (saving || next === value) {
      setOpen(false);
      return;
    }
    setSaving(true);
    try {
      await onChange(next);
      setOpen(false);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="project-access-control" ref={rootRef}>
      <button
        type="button"
        className={`project-access-trigger${open ? " open" : ""}`}
        aria-expanded={open}
        disabled={disabled || saving}
        onClick={() => setOpen((current) => !current)}
        title={`${workspaceName}：${projectAccessModeLabel(value)}（仅限此工作区）`}
      >
        <span>权限：{saving ? "保存中…" : projectAccessModeLabel(value)}</span>
        <IconChevronDown width={12} height={12} />
      </button>
      {open && (
        <div className="project-access-menu" role="menu" aria-label="项目 Agent 权限">
          <div className="project-access-head">
            <strong>应如何批准操作？</strong>
            <span>仅限「{workspaceName}」工作区</span>
          </div>
          {OPTIONS.map((option) => (
            <button
              type="button"
              role="menuitemradio"
              aria-checked={option.value === value}
              className={`project-access-option${option.value === value ? " selected" : ""}`}
              key={option.value}
              disabled={saving}
              onClick={() => void choose(option.value)}
            >
              <span className="project-access-option-copy">
                <strong>{option.label}</strong>
                <small>{option.description}</small>
              </span>
              {option.value === value && <IconCheck width={15} height={15} />}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
