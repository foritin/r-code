import { CheckCircle, Play, WarningCircle } from "@phosphor-icons/react";

export function SummaryBand({ scenario, onPlay }) {
  return (
    <section className="summary-band">
      <div className="summary-result">
        <span>结果</span>
        <h1>{scenario.summary.result}</h1>
        <button className="play-recap-button" onClick={onPlay}>
          <Play size={15} weight="fill" aria-hidden="true" />
          播放 {scenario.recapSeconds} 秒回顾
        </button>
      </div>
      <div className="summary-changes">
        <span>重要变化</span>
        <ul>
          {scenario.summary.changed.map((item) => <li key={item}><CheckCircle size={15} weight="fill" />{item}</li>)}
        </ul>
      </div>
      <div className="summary-attention">
        <span>需要注意</span>
        <p><WarningCircle size={16} />{scenario.summary.attention}</p>
      </div>
    </section>
  );
}
