import { useState } from "react";
import { useT } from "./settings";

export interface TabSummary {
  id: number;
  title: string;
  running: boolean;
}

interface Props {
  tabs: TabSummary[];
  activeId: number;
  onSelect: (id: number) => void;
  onNew: () => void;
  onClose: (id: number) => void;
  onRename: (id: number, title: string) => void;
  /** The schemas to pick from, and the one the active tab's names resolve in. */
  schemas: string[];
  schema: string | null;
  onSchemaChange: (schema: string) => void;
}

/**
 * The editor's tabs, each a console with its own session, and the schema the
 * active one's unqualified names resolve in.
 */
export function SqlTabs({
  tabs,
  activeId,
  onSelect,
  onNew,
  onClose,
  onRename,
  schemas,
  schema,
  onSchemaChange,
}: Props) {
  const t = useT();
  const [renaming, setRenaming] = useState<number | null>(null);
  const [title, setTitle] = useState("");
  // A schema set in SQL may not be in the list yet.
  const choices = schema && !schemas.includes(schema) ? [schema, ...schemas] : schemas;

  function finishRename() {
    if (renaming !== null && title.trim()) onRename(renaming, title.trim());
    setRenaming(null);
  }

  return (
    <div className="sql-tabs">
      <div className="sql-tab-list" role="tablist" aria-label={t.tabs.label}>
        {tabs.map((tab) => (
          <div
            key={tab.id}
            role="tab"
            aria-selected={tab.id === activeId}
            className={tab.id === activeId ? "sql-tab selected" : "sql-tab"}
            onClick={() => onSelect(tab.id)}
            onDoubleClick={() => {
              setRenaming(tab.id);
              setTitle(tab.title);
            }}
            // A middle click closes the tab, as in a browser.
            onAuxClick={(e) => e.button === 1 && tabs.length > 1 && onClose(tab.id)}
            title={t.tabs.renameTitle}
          >
            {tab.running && <span className="sql-tab-running" aria-label={t.workspace.running} />}
            {renaming === tab.id ? (
              <input
                className="sql-tab-rename"
                value={title}
                autoFocus
                onChange={(e) => setTitle(e.target.value)}
                onBlur={finishRename}
                onKeyDown={(e) => {
                  if (e.key === "Enter") finishRename();
                  if (e.key === "Escape") setRenaming(null);
                }}
                onClick={(e) => e.stopPropagation()}
                aria-label={t.tabs.rename}
              />
            ) : (
              <span className="sql-tab-title">{tab.title}</span>
            )}
            {tabs.length > 1 && (
              <button
                className="sql-tab-close"
                onClick={(e) => {
                  e.stopPropagation();
                  onClose(tab.id);
                }}
                title={t.tabs.close}
                aria-label={t.tabs.close}
              >
                ×
              </button>
            )}
          </div>
        ))}
        <button className="sql-tab-new" onClick={onNew} title={t.tabs.newTitle} aria-label={t.tabs.newTitle}>
          +
        </button>
      </div>
      <label className="schema-picker" title={t.tabs.schemaTitle}>
        {t.tabs.schema}
        <select value={schema ?? ""} onChange={(e) => onSchemaChange(e.target.value)}>
          {schema === null && <option value="">{t.tabs.noSchema}</option>}
          {choices.map((name) => (
            <option key={name} value={name}>
              {name}
            </option>
          ))}
        </select>
      </label>
    </div>
  );
}
