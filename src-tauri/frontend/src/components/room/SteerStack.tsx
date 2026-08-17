export interface SteerStackItem {
  id: string;
  text: string;
}

/**
 * 运行中的引导浮窗栈。
 *
 * 数据按时间顺序传入（最旧在前）。视觉上最新的引导卡片位于底部、靠近输入框，
 * 更早的引导依次向上堆叠。容器绝对定位，不占用时间线布局；挂载方需要把本组件
 * 放在 `.room-composer-region`（已 `position: relative`）内，`.steer-stack` 通过
 * `bottom: 100%` 贴在输入区顶部上方，与输入框之间不留缝隙。
 */
export function SteerStack({ items }: { items: readonly SteerStackItem[] }) {
  if (items.length === 0) return null;
  return (
    <div className="steer-stack" aria-label="运行中的引导" aria-live="polite">
      {items.map((item) => (
        <div className="steer-window" key={item.id} title={item.text}>
          <span className="steer-window-mark">引导</span>
          <span className="steer-window-text">{item.text}</span>
        </div>
      ))}
    </div>
  );
}
