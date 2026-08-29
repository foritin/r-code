/**
 * 模型配置抽屉的行/返回按钮（F-maint-06 收敛）。
 *
 * CodexModelConfiguration 与 ModelSwitcher 原先各有一份逐字一致的
 * ConfigRow/ConfigBack；提取到此处，样式契约（ring-inset 等）单点维护。
 */
export function ConfigRow({ label, value, onSelect }: { label: string; value: string; onSelect: () => void }) {
  return (
    <button className="model-config-row ring-inset" type="button" onClick={onSelect}>
      <span>{label}</span><strong title={value}>{value}</strong><span aria-hidden="true">›</span>
    </button>
  );
}

export function ConfigBack({ title, onBack }: { title: string; onBack: () => void }) {
  return (
    <button className="model-config-back ring-inset" type="button" onClick={onBack}>
      <span aria-hidden="true">←</span><strong>{title}</strong>
    </button>
  );
}
