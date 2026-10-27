import { KeyboardEvent, PointerEvent, useRef } from "react";

interface Props {
  /** "vertical" for a bar between side-by-side panels, "horizontal" for stacked ones. */
  orientation: "vertical" | "horizontal";
  /** The size of the panel it resizes, in px. */
  size: number;
  /** The allowed sizes, read when a drag starts, since they depend on the window. */
  bounds: () => [min: number, max: number];
  /** 1 when dragging right or down grows the panel, -1 when it shrinks it. */
  grows: 1 | -1;
  onResize: (size: number) => void;
  /** Double-click: back to the default size. */
  onReset: () => void;
  label: string;
}

/** A bar to drag, or focus and move with the arrow keys, to resize a panel. */
export function Splitter({ orientation, size, bounds, grows, onResize, onReset, label }: Props) {
  const drag = useRef<{ start: number; size: number; min: number; max: number } | null>(null);
  const position = (e: PointerEvent) => (orientation === "vertical" ? e.clientX : e.clientY);
  const clamp = (value: number, [min, max]: [number, number]) =>
    Math.round(Math.min(Math.max(value, min), Math.max(min, max)));

  function start(e: PointerEvent<HTMLDivElement>) {
    if (e.button !== 0) return;
    e.preventDefault();
    const [min, max] = bounds();
    drag.current = { start: position(e), size, min, max };
    e.currentTarget.setPointerCapture(e.pointerId);
    document.body.classList.add(`resizing-${orientation}`);
  }

  function move(e: PointerEvent) {
    const d = drag.current;
    if (!d) return;
    onResize(clamp(d.size + grows * (position(e) - d.start), [d.min, d.max]));
  }

  function end() {
    drag.current = null;
    document.body.classList.remove(`resizing-${orientation}`);
  }

  function key(e: KeyboardEvent) {
    const [back, forward] = orientation === "vertical" ? ["ArrowLeft", "ArrowRight"] : ["ArrowUp", "ArrowDown"];
    const step = e.shiftKey ? 64 : 16;
    if (e.key !== back && e.key !== forward) return;
    e.preventDefault();
    onResize(clamp(size + grows * (e.key === forward ? step : -step), bounds()));
  }

  return (
    <div
      className={`splitter ${orientation}`}
      role="separator"
      aria-orientation={orientation}
      aria-label={label}
      aria-valuenow={size}
      title={label}
      tabIndex={0}
      onPointerDown={start}
      onPointerMove={move}
      onPointerUp={end}
      onPointerCancel={end}
      onDoubleClick={onReset}
      onKeyDown={key}
    />
  );
}
