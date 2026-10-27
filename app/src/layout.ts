import { useEffect, useState } from "react";

/** Which panels show and how big they are; kept across launches. */
export interface Layout {
  treeVisible: boolean;
  /** Schema tree width, in px. */
  treeWidth: number;
  resultsVisible: boolean;
  /** Editor height while the results show, in px. */
  editorHeight: number;
  /** Snapshot panel width, in px. */
  snapshotWidth: number;
}

const STORAGE_KEY = "dolmen.layout";

export function defaultLayout(): Layout {
  return {
    treeVisible: true,
    treeWidth: 260,
    resultsVisible: true,
    editorHeight: Math.round(window.innerHeight * 0.35),
    snapshotWidth: 640,
  };
}

export function useLayout() {
  const [layout, setLayout] = useState(load);

  useEffect(() => {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(layout));
    } catch {
      // Storage can be unavailable; the layout then lasts until the app quits.
    }
  }, [layout]);

  const update = (change: Partial<Layout>) => setLayout((current) => ({ ...current, ...change }));
  const toggle = (panel: "treeVisible" | "resultsVisible") =>
    setLayout((current) => ({ ...current, [panel]: !current[panel] }));
  return { layout, update, toggle };
}

function load(): Layout {
  const defaults = defaultLayout();
  try {
    const stored = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "{}");
    const pick = <K extends keyof Layout>(key: K): Layout[K] =>
      typeof stored[key] === typeof defaults[key] ? stored[key] : defaults[key];
    return {
      treeVisible: pick("treeVisible"),
      treeWidth: pick("treeWidth"),
      resultsVisible: pick("resultsVisible"),
      editorHeight: pick("editorHeight"),
      snapshotWidth: pick("snapshotWidth"),
    };
  } catch {
    return defaults;
  }
}
