import { MouseEvent, useCallback, useEffect, useMemo, useState } from "react";
import { RedisKey, errorMessage, redis } from "./api";
import { ContextMenu, MenuItem } from "./ContextMenu";
import { useT } from "./settings";

/** How many keys one read lists; a narrower pattern finds the rest. */
const LIMIT = 2000;

const KIND_BADGES: Record<string, string> = {
  string: "S",
  hash: "H",
  list: "L",
  set: "SE",
  zset: "Z",
  stream: "X",
  "ReJSON-RL": "J",
};

interface Props {
  /** The session whose database is listed: the active tab's. */
  sessionId: () => Promise<number>;
  /** Changes when the active tab or its database does, to read the keys again. */
  scope: string;
  /** Adds a command at the end of the editor, and runs it when `run` is set. */
  insert: (command: string, run: boolean) => void;
  /** In px. */
  width: number;
}

/** Keys grouped by `:`, as `user:1:name` sits in `user` › `1`. */
interface Group {
  groups: Map<string, Group>;
  keys: RedisKey[];
  count: number;
}

function groupKeys(keys: RedisKey[]): Group {
  const root: Group = { groups: new Map(), keys: [], count: 0 };
  for (const key of keys) {
    const parts = key.name.split(":");
    let group = root;
    group.count++;
    for (const part of parts.slice(0, -1)) {
      let next = group.groups.get(part);
      if (!next) {
        next = { groups: new Map(), keys: [], count: 0 };
        group.groups.set(part, next);
      }
      next.count++;
      group = next;
    }
    group.keys.push(key);
  }
  return root;
}

/**
 * A Redis database's keys, read with SCAN, grouped by their `:` prefixes.
 * Double-click a key to show its value; right-click for more.
 */
export function RedisTree({ sessionId, scope, insert, width }: Props) {
  const t = useT();
  const [pattern, setPattern] = useState("");
  const [keys, setKeys] = useState<{ keys: RedisKey[]; complete: boolean } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [menu, setMenu] = useState<{ x: number; y: number; items: MenuItem[] } | null>(null);

  const load = useCallback(
    async (match: string) => {
      setLoading(true);
      try {
        setKeys(await redis.keys(await sessionId(), match, LIMIT));
        setError(null);
      } catch (err) {
        setError(errorMessage(err, t));
      } finally {
        setLoading(false);
      }
    },
    [sessionId, t],
  );
  useEffect(() => {
    load(pattern);
    // A new pattern loads on Enter; another tab or database loads at once.
  }, [scope]);

  const tree = useMemo(() => groupKeys(keys?.keys ?? []), [keys]);
  const toggle = (path: string) =>
    setOpen((current) => {
      const next = new Set(current);
      if (!next.delete(path)) next.add(path);
      return next;
    });
  const openMenu = (event: MouseEvent, key: RedisKey) => {
    event.preventDefault();
    const name = key.quoted;
    setMenu({
      x: event.clientX,
      y: event.clientY,
      items: [
        { label: t.tree.keys.showValue, onSelect: () => insert(key.show, true) },
        { label: t.tree.keys.showTtl, onSelect: () => insert(`TTL ${name}`, true) },
        { separator: true },
        {
          label: t.tree.keys.copyName,
          onSelect: () => {
            navigator.clipboard.writeText(key.name).catch((err) => console.error("copy failed", err));
          },
        },
        { separator: true },
        // Like the schema tree's drops, deleting goes into the editor for review.
        { label: t.tree.keys.delete, danger: true, onSelect: () => insert(`DEL ${name}`, false) },
      ],
    });
  };
  const closeMenu = useCallback(() => setMenu(null), []);

  function renderGroup(group: Group, path: string, depth: number) {
    return (
      <>
        {[...group.groups.entries()]
          .sort(([a], [b]) => a.localeCompare(b))
          .map(([name, child]) => {
            const childPath = `${path}${name}:`;
            const isOpen = open.has(childPath) || pattern.trim() !== "";
            return (
              <div key={`g:${childPath}`}>
                <div className="tree-row" style={{ paddingLeft: 4 + depth * 16 }} onClick={() => toggle(childPath)}>
                  <span className="tree-caret">
                    <svg viewBox="0 0 16 16" className={isOpen ? "open" : undefined} aria-hidden>
                      <path d="M6 3.5 10.5 8 6 12.5" />
                    </svg>
                  </span>
                  <span className="tree-label">{name}</span>
                  <span className="tree-detail">{child.count}</span>
                </div>
                {isOpen && renderGroup(child, childPath, depth + 1)}
              </div>
            );
          })}
        {[...group.keys]
          .sort((a, b) => a.name.localeCompare(b.name))
          .map((key) => (
            <div
              key={`k:${key.name}`}
              className="tree-row"
              style={{ paddingLeft: 4 + depth * 16 }}
              onDoubleClick={() => insert(key.show, true)}
              onContextMenu={(e) => openMenu(e, key)}
              title={`${key.name}\n${key.kind}\n${t.tree.keys.doubleClickToShow}`}
            >
              <span className="tree-caret" />
              <span className="tree-badge">{KIND_BADGES[key.kind] ?? "?"}</span>
              <span className="tree-label">{key.name.slice(path.length) || key.name}</span>
            </div>
          ))}
      </>
    );
  }

  return (
    <aside className="schema-tree" style={{ width }}>
      <div className="schema-tree-header">
        <input
          className="schema-filter"
          placeholder={t.tree.keys.filter}
          value={pattern}
          onChange={(e) => setPattern(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && load(pattern)}
        />
        <button onClick={() => load(pattern)} disabled={loading} title={t.tree.keys.reload}>
          ↻
        </button>
      </div>
      <div className="schema-tree-body">
        {error && <p className="error-text tree-message">{error}</p>}
        {!keys && !error && <p className="muted tree-message">{loading ? t.common.loading : t.tree.notLoaded}</p>}
        {keys && keys.keys.length === 0 && <p className="muted tree-message">{t.tree.keys.none}</p>}
        {keys && !keys.complete && <p className="muted tree-message">{t.tree.keys.more(keys.keys.length)}</p>}
        {keys && renderGroup(tree, "", 0)}
      </div>
      {menu && <ContextMenu {...menu} onClose={closeMenu} />}
    </aside>
  );
}
