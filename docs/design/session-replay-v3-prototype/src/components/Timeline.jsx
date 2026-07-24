import {
  CaretLeft,
  CaretRight,
  Clock,
  Pause,
  Play,
  SkipBack,
  SkipForward,
} from "@phosphor-icons/react";
import { formatDuration } from "../data.js";
import { EventIcon } from "./ui.jsx";

const lanes = [
  ["intent", "意图与对话"],
  ["actions", "动作与应用"],
  ["artifacts", "产物与变化"],
  ["risk", "决策、风险与验证"],
];

export function eventPosition(event, scenario, timeMode) {
  if (timeMode === "story") return (event.story / scenario.recapSeconds) * 100;
  return (event.actual / scenario.actualSeconds) * 100;
}

export function Timeline({ scenario, event, index, progress, playing, speed, timeMode, onPlaying, onProgress, onEvent, onSpeed, onTimeMode }) {
  const activeSeconds = timeMode === "story" ? progress : (progress / scenario.recapSeconds) * scenario.actualSeconds;
  const totalSeconds = timeMode === "story" ? scenario.recapSeconds : scenario.actualSeconds;
  const playheadPercent = timeMode === "story"
    ? (progress / scenario.recapSeconds) * 100
    : (event.actual / scenario.actualSeconds) * 100;
  const seekIndex = (next) => {
    const safe = Math.max(0, Math.min(scenario.events.length - 1, next));
    onEvent(safe);
  };
  return (
    <footer className="semantic-timeline">
      <div className="timeline-controls">
        <button className="icon-button" onClick={() => seekIndex(0)} aria-label="First moment"><SkipBack size={16} /></button>
        <button className="icon-button" onClick={() => seekIndex(index - 1)} disabled={index === 0} aria-label="Previous moment"><CaretLeft size={16} /></button>
        <button className="player-button" onClick={() => onPlaying(!playing)}>
          {playing ? <Pause size={16} weight="fill" /> : <Play size={16} weight="fill" />}
          {playing ? "暂停" : progress >= scenario.recapSeconds ? "重新播放" : "播放"}
        </button>
        <button className="icon-button" onClick={() => seekIndex(index + 1)} disabled={index === scenario.events.length - 1} aria-label="Next moment"><CaretRight size={16} /></button>
        <button className="icon-button" onClick={() => seekIndex(scenario.events.length - 1)} aria-label="Last moment"><SkipForward size={16} /></button>
        <time>{formatDuration(activeSeconds)} / {formatDuration(totalSeconds)}</time>
      </div>
      <div className="timeline-body">
        <div className="timeline-mode">
          <Clock size={14} />
          <button className={timeMode === "story" ? "active" : ""} onClick={() => onTimeMode("story")}>故事时间</button>
          <button className={timeMode === "real" ? "active" : ""} onClick={() => onTimeMode("real")}>真实时间</button>
        </div>
        <div className="timeline-lanes">
          {lanes.map(([lane, label]) => (
            <div className="timeline-lane" key={lane}>
              <span>{label}</span>
              <div>
                {scenario.events.filter((item) => item.lane === lane).map((item) => {
                  const eventIndex = scenario.events.indexOf(item);
                  return (
                    <button
                      key={item.id}
                      className={`timeline-marker marker-${item.level} ${eventIndex === index ? "active" : ""}`}
                      style={{ left: `${eventPosition(item, scenario, timeMode)}%` }}
                      onClick={() => onEvent(eventIndex)}
                      title={`${formatDuration(timeMode === "story" ? item.story : item.actual)} · ${item.label}`}
                    >
                      <EventIcon type={item.type} size={11} weight="bold" />
                    </button>
                  );
                })}
              </div>
            </div>
          ))}
          <div className="playhead" style={{ left: `${playheadPercent}%` }} />
        </div>
        <input
          className="timeline-range"
          type="range"
          min="0"
          max={scenario.recapSeconds}
          step="0.1"
          value={progress}
          onChange={(e) => onProgress(Number(e.target.value))}
          aria-label="Replay timeline"
        />
        <div className="coverage-band" aria-label="Capture coverage timeline">
          {scenario.coverage.map((part, partIndex) => (
            <i key={`${part.level}-${partIndex}`} className={`coverage-${part.level}`} style={{ width: `${part.width}%` }} title={`${part.level} ${part.width}%`} />
          ))}
        </div>
      </div>
      <select value={speed} onChange={(e) => onSpeed(Number(e.target.value))} aria-label="Playback speed">
        {[1, 2, 4, 8, 16].map((value) => <option key={value} value={value}>{value}×</option>)}
      </select>
    </footer>
  );
}
