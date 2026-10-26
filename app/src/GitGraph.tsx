import { Commit } from "./api";

const ROW_HEIGHT = 28;
const LANE_WIDTH = 14;
const LANE_COLORS = ["#3b5bdb", "#2f9e44", "#e8590c", "#ae3ec9", "#1098ad", "#d6336c", "#5c940d"];

interface Props {
  commits: Commit[];
  selected: string | null;
  onSelect: (commit: Commit) => void;
}

/**
 * A commit graph like `git log --graph`: one row per commit, newest first,
 * with lines from each commit down to its parents. Filled dots mark commits
 * that changed the schema snapshot.
 */
export function GitGraph({ commits, selected, onSelect }: Props) {
  const rows = layout(commits);
  const width = (Math.max(1, ...rows.map((row) => row.width)) + 1) * LANE_WIDTH;

  return (
    <div className="git-graph">
      {rows.map((row) => {
        const { commit } = row;
        return (
          <button
            key={commit.hash}
            className={`graph-row ${commit.hash === selected ? "selected" : ""} ${commit.changesSnapshot ? "" : "dim"}`}
            onClick={() => onSelect(commit)}
            title={`${commit.hash}\n${commit.author}, ${new Date(commit.date).toLocaleString()}`}
          >
            <svg width={width} height={ROW_HEIGHT} className="graph-lanes" aria-hidden>
              {row.segments.map((segment, i) => (
                <line
                  key={i}
                  x1={x(segment.from)}
                  y1={segment.top ? 0 : ROW_HEIGHT / 2}
                  x2={x(segment.to)}
                  y2={segment.top ? ROW_HEIGHT / 2 : ROW_HEIGHT}
                  stroke={color(segment.color)}
                  strokeWidth={2}
                />
              ))}
              <circle
                cx={x(row.lane)}
                cy={ROW_HEIGHT / 2}
                r={commit.head ? 5.5 : 4.5}
                fill={commit.changesSnapshot ? color(row.lane) : "var(--surface)"}
                stroke={color(row.lane)}
                strokeWidth={2}
              />
            </svg>
            <span className="graph-text">
              {commit.refs.map((ref) => (
                <span key={ref} className={`graph-ref ${ref.startsWith("tag: ") ? "tag" : ""} ${commit.head && !ref.includes("/") && !ref.startsWith("tag: ") ? "head" : ""}`}>
                  {ref.replace(/^tag: /, "")}
                </span>
              ))}
              <span className="graph-subject">{commit.subject}</span>
            </span>
            <span className="graph-meta">
              {commit.author} · {relativeDate(commit.date)} · <code>{commit.hash.slice(0, 7)}</code>
            </span>
          </button>
        );
      })}
    </div>
  );
}

interface Segment {
  from: number;
  to: number;
  /** The upper half of the row (into the commit) or the lower half (out of it). */
  top: boolean;
  color: number;
}

interface Row {
  commit: Commit;
  lane: number;
  segments: Segment[];
  /** Lanes in use on this row. */
  width: number;
}

/**
 * Assigns each commit a lane. A lane holds the hash it is waiting for: the
 * parent of the commit drawn above it. Commits arrive newest first and never
 * before their children, as `git log --topo-order` gives them.
 */
function layout(commits: Commit[]): Row[] {
  const known = new Set(commits.map((c) => c.hash));
  let lanes: (string | null)[] = [];
  const rows: Row[] = [];

  for (const commit of commits) {
    let lane = lanes.indexOf(commit.hash);
    const hasChild = lane !== -1;
    if (!hasChild) {
      // A branch tip: nothing above leads here, so it starts a lane.
      lane = freeLane(lanes);
      lanes[lane] = commit.hash;
    }
    const before = [...lanes];
    const segments: Segment[] = [];

    // Upper half: lanes passing by, and lanes of children that end at this commit.
    before.forEach((hash, i) => {
      if (hash === null) return;
      if (hash !== commit.hash) {
        segments.push({ from: i, to: i, top: true, color: i });
      } else if (i !== lane) {
        segments.push({ from: i, to: lane, top: true, color: i });
        lanes[i] = null;
      } else if (hasChild) {
        segments.push({ from: lane, to: lane, top: true, color: lane });
      }
    });

    // Lower half: the first parent continues this lane, other parents join their
    // lane or start one, and every other lane passes by.
    const parents = commit.parents.filter((parent) => known.has(parent));
    lanes[lane] = parents[0] ?? null;
    for (const parent of parents.slice(1)) {
      if (!lanes.includes(parent)) lanes[freeLane(lanes)] = parent;
    }
    lanes.forEach((hash, i) => {
      if (hash === null) return;
      if (parents.includes(hash)) {
        segments.push({ from: lane, to: i, top: false, color: i });
      }
      if (i !== lane && before[i] === hash) {
        segments.push({ from: i, to: i, top: false, color: i });
      }
    });

    const width = Math.max(before.length, lanes.length);
    while (lanes.length > 0 && lanes[lanes.length - 1] === null) lanes = lanes.slice(0, -1);
    rows.push({ commit, lane, segments, width });
  }
  return rows;
}

function freeLane(lanes: (string | null)[]) {
  const free = lanes.indexOf(null);
  return free === -1 ? lanes.length : free;
}

function x(lane: number) {
  return LANE_WIDTH / 2 + lane * LANE_WIDTH + 2;
}

function color(lane: number) {
  return LANE_COLORS[lane % LANE_COLORS.length];
}

function relativeDate(iso: string) {
  const seconds = (Date.now() - new Date(iso).getTime()) / 1000;
  const steps: [number, string][] = [
    [60, "s"],
    [60, "m"],
    [24, "h"],
    [30, "d"],
    [12, "mo"],
  ];
  let value = seconds;
  for (const [size, unit] of steps) {
    if (value < size) return `${Math.max(0, Math.floor(value))}${unit} ago`;
    value /= size;
  }
  return `${Math.floor(value)}y ago`;
}
