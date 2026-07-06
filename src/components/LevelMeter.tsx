type Status = "idle" | "running" | "stale" | "error";

interface LevelMeterProps {
  label: string;
  rms: number;
  status: Status;
}

// Real speech/system audio RMS values are usually well under 0.3 (see the
// Phase 1/2 manual verification logs), so the bar is scaled up rather than
// mapped 1:1 — otherwise everything would sit near the left edge and the
// meter would look dead even while correctly capturing quiet speech.
const DISPLAY_SCALE = 3;

function clamp01(value: number): number {
  return Math.max(0, Math.min(1, value));
}

export function LevelMeter({ label, rms, status }: LevelMeterProps) {
  const widthPercent = clamp01(rms * DISPLAY_SCALE) * 100;

  return (
    <div className="level-meter">
      <div className="level-meter-header">
        <span className={`status-dot status-dot--${status}`} title={`status: ${status}`} />
        <span className="level-meter-label">{label}</span>
      </div>
      <div className="level-meter-track">
        <div className="level-meter-fill" style={{ width: `${widthPercent}%` }} />
      </div>
    </div>
  );
}
