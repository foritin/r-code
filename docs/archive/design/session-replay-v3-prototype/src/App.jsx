import { CheckCircle, X } from "@phosphor-icons/react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { scenarios } from "./data.js";
import { Explore } from "./components/Explore.jsx";
import { Header } from "./components/Header.jsx";
import { Recap } from "./components/Recap.jsx";
import { Timeline } from "./components/Timeline.jsx";
import { Verify } from "./components/Verify.jsx";

export function App() {
  const [scenarioId, setScenarioId] = useState("research");
  const scenario = useMemo(() => scenarios.find((item) => item.id === scenarioId) ?? scenarios[0], [scenarioId]);
  const [depth, setDepth] = useState("recap");
  const [index, setIndex] = useState(scenario.events.length - 1);
  const [progress, setProgress] = useState(scenario.recapSeconds);
  const [playing, setPlaying] = useState(false);
  const [speed, setSpeed] = useState(1);
  const [timeMode, setTimeMode] = useState("story");
  const [menuOpen, setMenuOpen] = useState(false);
  const [toast, setToast] = useState("");
  const [answer, setAnswer] = useState("");
  const frameRef = useRef(null);
  const previousRef = useRef(null);

  const event = scenario.events[index] ?? scenario.events[0];

  const selectEvent = useCallback((nextIndex) => {
    const safe = Math.max(0, Math.min(scenario.events.length - 1, nextIndex));
    setPlaying(false);
    setIndex(safe);
    setProgress(scenario.events[safe].story);
    setAnswer("");
  }, [scenario]);

  const seek = useCallback((nextProgress) => {
    const safe = Math.max(0, Math.min(scenario.recapSeconds, nextProgress));
    setPlaying(false);
    setProgress(safe);
    const nextIndex = scenario.events.reduce((latest, item, itemIndex) => item.story <= safe ? itemIndex : latest, 0);
    setIndex(nextIndex);
    setAnswer("");
  }, [scenario]);

  const startRecap = useCallback(() => {
    setDepth("recap");
    setIndex(0);
    setProgress(0);
    setPlaying(true);
    setAnswer("");
  }, []);

  useEffect(() => {
    if (!playing) {
      previousRef.current = null;
      if (frameRef.current) cancelAnimationFrame(frameRef.current);
      return undefined;
    }
    const tick = (now) => {
      const previous = previousRef.current ?? now;
      previousRef.current = now;
      setProgress((current) => {
        const next = current + ((now - previous) / 1000) * speed;
        if (next >= scenario.recapSeconds) {
          setPlaying(false);
          setIndex(scenario.events.length - 1);
          return scenario.recapSeconds;
        }
        const nextIndex = scenario.events.reduce((latest, item, itemIndex) => item.story <= next ? itemIndex : latest, 0);
        setIndex(nextIndex);
        return next;
      });
      frameRef.current = requestAnimationFrame(tick);
    };
    frameRef.current = requestAnimationFrame(tick);
    return () => {
      if (frameRef.current) cancelAnimationFrame(frameRef.current);
      previousRef.current = null;
    };
  }, [playing, speed, scenario]);

  useEffect(() => {
    if (!toast) return undefined;
    const timer = window.setTimeout(() => setToast(""), 3200);
    return () => window.clearTimeout(timer);
  }, [toast]);

  useEffect(() => {
    const onKey = (keyboardEvent) => {
      if (["INPUT", "SELECT", "TEXTAREA"].includes(document.activeElement?.tagName)) return;
      if (keyboardEvent.key === " ") {
        keyboardEvent.preventDefault();
        setPlaying((value) => !value);
      }
      if (keyboardEvent.key === "ArrowLeft") selectEvent(index - 1);
      if (keyboardEvent.key === "ArrowRight") selectEvent(index + 1);
      if (keyboardEvent.key === "Escape") setMenuOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [index, selectEvent]);

  const changeScenario = (nextId) => {
    const next = scenarios.find((item) => item.id === nextId);
    if (!next) return;
    setScenarioId(nextId);
    setIndex(next.events.length - 1);
    setProgress(next.recapSeconds);
    setPlaying(false);
    setAnswer("");
    setMenuOpen(false);
    setToast(`已切换到“${next.shortTitle}”；三层深度共用同一份证据。`);
  };

  const askReplay = (question) => {
    if (event.level === "observed") {
      setAnswer(`记录只能确认“${event.label}”出现在观察证据中，无法确认应用内部为什么这样做。`);
    } else if (event.level === "inferred") {
      setAnswer(`这项判断由 ${event.evidence.length} 条证据归纳而来；记录不包含 Agent 的隐藏推理。`);
    } else {
      setAnswer(`“${event.label}”由 ${event.evidence.join("、")} 直接支持。问题“${question}”的回答仅基于这些记录。`);
    }
  };

  const sharedProps = {
    scenario,
    event,
    index,
    onEvent: selectEvent,
    onDepth: setDepth,
    onToast: setToast,
    answer,
    onAsk: askReplay,
  };

  return (
    <div className={`replay-app depth-${depth}`} onClick={() => menuOpen && setMenuOpen(false)}>
      <div className="header-stack" onClick={(clickEvent) => clickEvent.stopPropagation()}>
        <Header
          scenario={scenario}
          depth={depth}
          onDepth={setDepth}
          menuOpen={menuOpen}
          onMenu={() => setMenuOpen((value) => !value)}
          onScenario={changeScenario}
          onToast={setToast}
        />
      </div>
      <div className="depth-content">
        {depth === "recap" ? <Recap {...sharedProps} onPlay={startRecap} /> : null}
        {depth === "explore" ? <Explore {...sharedProps} /> : null}
        {depth === "verify" ? <Verify {...sharedProps} /> : null}
      </div>
      <Timeline
        scenario={scenario}
        event={event}
        index={index}
        progress={progress}
        playing={playing}
        speed={speed}
        timeMode={timeMode}
        onPlaying={(value) => {
          if (value && progress >= scenario.recapSeconds) {
            setProgress(0);
            setIndex(0);
          }
          setPlaying(value);
        }}
        onProgress={seek}
        onEvent={selectEvent}
        onSpeed={setSpeed}
        onTimeMode={setTimeMode}
      />
      {toast ? (
        <div className="toast" role="status">
          <CheckCircle size={18} weight="fill" />
          <span>{toast}</span>
          <button onClick={() => setToast("")} aria-label="Dismiss notification"><X size={15} /></button>
        </div>
      ) : null}
    </div>
  );
}
