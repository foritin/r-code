/**
 * 态势带 —— 5 个 gauge 卡 + 舰队示波器（40 柱，waveB 呼吸）。
 * 结构照 fusion-obsidian.html:235-253, 738-748。
 */
import { waveHeights, type DeckGauges } from "../../lib/deck";

export function SituationBand({
  gauges,
  eventSeed,
}: {
  gauges: DeckGauges;
  eventSeed: number;
}) {
  const bars = waveHeights(eventSeed, 40);
  return (
    <div className="situation">
      <Gauge k="进行中" v={gauges.running} variant={gauges.running > 0 ? "live" : ""} />
      <Gauge k="待处理" v={gauges.needsYou} variant={gauges.needsYou > 0 ? "hot" : ""} />
      <Gauge
        k="今日已验证"
        v={gauges.verifiedToday}
        variant={gauges.verifiedToday > 0 ? "good" : ""}
      />
      <Gauge k="涉及文件" v={gauges.filesInFlight} />
      <Gauge k="本周已接受" v={gauges.acceptedPerWeek} />
      <div className="oscillo">
        <div className="k">
          任务活动 <b>每分钟事件</b>
        </div>
        <div className="wave">
          {bars.map((h, i) => (
            <i
              key={i}
              style={{
                height: `${h}%`,
                animationDelay: `${((i * 0.13) % 2.6).toFixed(2)}s`,
              }}
            />
          ))}
        </div>
      </div>
    </div>
  );
}

function Gauge({
  k,
  v,
  variant = "",
}: {
  k: string;
  v: number;
  variant?: "" | "hot" | "live" | "good";
}) {
  return (
    <div className={`gauge${variant ? ` ${variant}` : ""}`}>
      <div className="k">{k}</div>
      <div className="v">{v}</div>
    </div>
  );
}
