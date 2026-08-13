import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import companionAtlas from "../../assets/companion/r-code-miku-v4.webp";
import type { CompanionMotion } from "../../store/companion";
import type { CompanionMood } from "./policy";
import "../../styles/companion-sprite.css";

export type CompanionSpriteState = CompanionMood | "sing" | "dance";

interface CompanionSpriteProps {
  motion: CompanionMotion;
  state: CompanionSpriteState;
}

interface SpriteSequence {
  columns: number;
  durations: readonly number[];
  frames?: readonly number[];
  loops: number;
  row: number;
  rows: number;
  sheet: string;
}

const CODEX_IDLE_TIMING = [280, 110, 110, 140, 140, 320] as const;
const slowIdleTiming = CODEX_IDLE_TIMING.map((duration) => duration * 6);
const sequence = (
  row: number,
  frameCount: number,
  duration: number,
  finalDuration: number,
): SpriteSequence => ({
  columns: 8,
  durations: Array.from({ length: frameCount }, (_, index) => (
    index === frameCount - 1 ? finalDuration : duration
  )),
  loops: 3,
  row,
  rows: 9,
  sheet: companionAtlas,
});

export const COMPANION_SPRITE_SEQUENCES: Record<CompanionSpriteState, SpriteSequence> = {
  idle: {
    columns: 8,
    durations: slowIdleTiming,
    loops: Number.POSITIVE_INFINITY,
    row: 0,
    rows: 9,
    sheet: companionAtlas,
  },
  attention: sequence(6, 6, 150, 260),
  error: sequence(5, 8, 140, 240),
  review: sequence(8, 6, 150, 280),
  success: sequence(3, 4, 140, 280),
  working: sequence(7, 6, 120, 220),
  // Hover gestures deliberately reuse the registered Codex-cell atlas. The previous performance
  // sheet changed the character silhouette by 30-50% between states, which looked like the whole
  // pet was being squeezed. Forward-and-return frame paths also avoid a hard last-to-first cut.
  sing: {
    columns: 8,
    durations: [110, 110, 120, 160, 120, 110, 110, 160],
    frames: [0, 1, 2, 3, 4, 5, 4, 2],
    loops: 2,
    row: 0,
    rows: 9,
    sheet: companionAtlas,
  },
  dance: {
    columns: 8,
    durations: [90, 90, 100, 120, 100, 90, 90, 120],
    frames: [0, 1, 2, 3, 4, 5, 4, 2],
    loops: 2,
    row: 0,
    rows: 9,
    sheet: companionAtlas,
  },
};

function framePosition(column: number, row: number, columns: number, rows: number): string {
  const x = (column / (columns - 1)) * 100;
  const y = rows === 1 ? 0 : (row / (rows - 1)) * 100;
  return `${x}% ${y}%`;
}

function useSystemReducedMotion(): boolean {
  const [reduced, setReduced] = useState(() => (
    window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false
  ));

  useEffect(() => {
    const query = window.matchMedia?.("(prefers-reduced-motion: reduce)");
    if (!query) return;
    const update = () => setReduced(query.matches);
    query.addEventListener?.("change", update);
    return () => query.removeEventListener?.("change", update);
  }, []);

  return reduced;
}

/** Codex-compatible, single-plane sprite playback with authored per-frame dwell times. */
export function CompanionSprite({ motion, state }: CompanionSpriteProps) {
  const stageRef = useRef<HTMLSpanElement>(null);
  const systemReduced = useSystemReducedMotion();
  const reduced = motion === "reduced" || (motion === "system" && systemReduced);
  const requestedSequence = COMPANION_SPRITE_SEQUENCES[state];

  const initialStyle = useMemo(() => {
    const firstColumn = requestedSequence.frames?.[0] ?? 0;
    return {
      backgroundImage: `url(${requestedSequence.sheet})`,
      backgroundPosition: framePosition(
        firstColumn,
        requestedSequence.row,
        requestedSequence.columns,
        requestedSequence.rows,
      ),
      backgroundSize: `${requestedSequence.columns * 100}% ${requestedSequence.rows * 100}%`,
    } as CSSProperties;
  }, [requestedSequence]);

  useEffect(() => {
    const stage = stageRef.current;
    if (!stage) return;

    let cancelled = false;
    let timer: number | null = null;
    const showFrame = (
      activeSequence: SpriteSequence,
      frame: number,
      activeState: CompanionSpriteState,
    ) => {
      const column = activeSequence.frames?.[frame] ?? frame;
      stage.style.backgroundImage = `url(${activeSequence.sheet})`;
      stage.style.backgroundPosition = framePosition(
        column,
        activeSequence.row,
        activeSequence.columns,
        activeSequence.rows,
      );
      stage.style.backgroundSize = `${activeSequence.columns * 100}% ${activeSequence.rows * 100}%`;
      stage.dataset.frameIndex = String(column);
      stage.dataset.activeSpriteState = activeState;
    };

    if (reduced) {
      showFrame(requestedSequence, 0, state);
      return;
    }

    let activeSequence = requestedSequence;
    let activeState = state;
    let frame = 0;
    let completedLoops = 0;
    showFrame(activeSequence, frame, activeState);

    const scheduleNext = () => {
      timer = window.setTimeout(() => {
        if (cancelled) return;
        frame += 1;
        if (frame >= activeSequence.durations.length) {
          completedLoops += 1;
          frame = 0;
          if (completedLoops >= activeSequence.loops) {
            // Codex plays transient state rows a bounded number of times, then settles into its
            // slow idle loop instead of leaving the character twitching indefinitely.
            activeSequence = COMPANION_SPRITE_SEQUENCES.idle;
            activeState = "idle";
            completedLoops = 0;
          }
        }
        showFrame(activeSequence, frame, activeState);
        scheduleNext();
      }, activeSequence.durations[frame] ?? 160);
    };

    scheduleNext();
    return () => {
      cancelled = true;
      if (timer !== null) window.clearTimeout(timer);
    };
  }, [reduced, requestedSequence, state]);

  return (
    <span
      ref={stageRef}
      className="companion-sprite-frame companion-sequence-frame"
      data-active-sprite-state={state}
      data-frame-index={0}
      data-sprite-state={state}
      aria-hidden="true"
      style={initialStyle}
    />
  );
}
