interface CaptureControlsProps {
  isRunning: boolean;
  isBusy: boolean;
  onStart: () => void;
  onStop: () => void;
}

export function CaptureControls({ isRunning, isBusy, onStart, onStop }: CaptureControlsProps) {
  return (
    <div className="row">
      <button onClick={onStart} disabled={isRunning || isBusy}>
        Start capture
      </button>
      <button onClick={onStop} disabled={!isRunning || isBusy}>
        Stop capture
      </button>
    </div>
  );
}
