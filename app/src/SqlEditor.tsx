import { useEffect, useRef, useState } from "react";
import { basicSetup } from "codemirror";
import { EditorView, keymap } from "@codemirror/view";
import { Compartment, EditorState, Prec } from "@codemirror/state";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { PostgreSQL, SQLNamespace, sql } from "@codemirror/lang-sql";
import { tags } from "@lezer/highlight";
import { Introspection } from "./api";
import { useT } from "./settings";

interface Props {
  initialValue: string;
  /** The database structure, for completing tables and columns; null until loaded. */
  introspection: Introspection | null;
  onRun: () => void;
  /** ⌘K: ask AI. */
  onAsk: () => void;
  onChange: (value: string) => void;
  /** Set to the editor view, so the parent can read text and selection when running. */
  viewRef: { current: EditorView | null };
  /** In px, or null to fill the space below the toolbar. */
  height: number | null;
  /** The schema unqualified names complete from; the database's default when unset. */
  defaultSchema?: string | null;
  /** A tab in the background keeps its editor, out of sight. */
  hidden?: boolean;
}

export function SqlEditor({
  initialValue,
  introspection,
  onRun,
  onAsk,
  onChange,
  viewRef,
  height,
  defaultSchema,
  hidden = false,
}: Props) {
  const parentRef = useRef<HTMLDivElement>(null);
  const t = useT();
  const [language] = useState(() => new Compartment());
  const [phrases] = useState(() => new Compartment());
  // The view is created once, so it reads the latest callbacks through a ref.
  const handlers = useRef({ onRun, onAsk, onChange });
  useEffect(() => {
    handlers.current = { onRun, onAsk, onChange };
  });

  useEffect(() => {
    const view = new EditorView({
      parent: parentRef.current!,
      state: EditorState.create({
        doc: initialValue,
        extensions: [
          Prec.highest(
            keymap.of([
              {
                key: "Mod-Enter",
                run: () => {
                  handlers.current.onRun();
                  return true;
                },
              },
              {
                key: "Mod-k",
                run: () => {
                  handlers.current.onAsk();
                  return true;
                },
              },
            ]),
          ),
          basicSetup,
          language.of(sqlLanguage(null, null)),
          phrases.of(EditorState.phrases.of(t.editorPhrases)),
          syntaxHighlighting(highlightStyle),
          theme,
          EditorView.updateListener.of((update) => {
            if (update.docChanged) handlers.current.onChange(update.state.doc.toString());
          }),
        ],
      }),
    });
    viewRef.current = view;
    view.focus();
    return () => {
      view.destroy();
      viewRef.current = null;
    };
    // The view owns the document after creation; later initialValue changes are ignored.
  }, []);

  useEffect(() => {
    viewRef.current?.dispatch({
      effects: language.reconfigure(sqlLanguage(introspection, defaultSchema ?? null)),
    });
  }, [introspection, defaultSchema, language, viewRef]);

  // Back in sight, the editor measures itself again and takes the focus.
  useEffect(() => {
    const view = viewRef.current;
    if (!hidden && view) {
      view.requestMeasure();
      view.focus();
    }
  }, [hidden, viewRef]);

  // CodeMirror's own labels, such as the search panel's, follow the app's language.
  useEffect(() => {
    viewRef.current?.dispatch({ effects: phrases.reconfigure(EditorState.phrases.of(t.editorPhrases)) });
  }, [t, phrases, viewRef]);

  return (
    <div
      ref={parentRef}
      className={height === null ? "editor fill" : "editor"}
      style={hidden ? { display: "none" } : height === null ? undefined : { height }}
    />
  );
}

function sqlLanguage(introspection: Introspection | null, defaultSchema: string | null) {
  return sql({
    dialect: PostgreSQL,
    upperCaseKeywords: true,
    schema: introspection ? namespace(introspection) : undefined,
    defaultSchema: defaultSchema ?? introspection?.defaultSchema ?? undefined,
  });
}

/** `{ schema: { table: [columns] } }` for tables and views, so `table` and `schema.table` complete. */
function namespace({ database }: Introspection): SQLNamespace {
  const schemas: Record<string, Record<string, string[]>> = {};
  for (const schema of database.schemas) {
    const relations: Record<string, string[]> = {};
    for (const relation of [...schema.tables, ...schema.views]) {
      relations[relation.name] = relation.columns.map((column) => column.name);
    }
    schemas[schema.name] = relations;
  }
  return schemas;
}

// Colors come from CSS variables in App.css, so light and dark mode follow the app.
const theme = EditorView.theme({
  "&": { height: "100%", backgroundColor: "var(--surface)", color: "var(--text)" },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": { fontFamily: "var(--mono)", fontSize: "13px", lineHeight: "1.6" },
  ".cm-content": { caretColor: "var(--text)" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--text)" },
  ".cm-gutters": { backgroundColor: "var(--surface)", color: "var(--muted)", border: "none" },
  ".cm-activeLine, .cm-activeLineGutter": { backgroundColor: "var(--active-line)" },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection":
    { backgroundColor: "var(--selection)" },
  ".cm-selectionMatch": { backgroundColor: "var(--selection-match)" },
  "&.cm-focused .cm-matchingBracket": { backgroundColor: "var(--selection-match)", outline: "none" },
  ".cm-tooltip": {
    border: "1px solid var(--border)",
    borderRadius: "6px",
    backgroundColor: "var(--surface)",
    color: "var(--text)",
  },
  ".cm-tooltip-autocomplete > ul": { fontFamily: "var(--mono)", fontSize: "12px" },
  ".cm-tooltip-autocomplete > ul > li[aria-selected]": {
    backgroundColor: "var(--accent)",
    color: "var(--accent-text)",
  },
});

const highlightStyle = HighlightStyle.define([
  { tag: tags.keyword, color: "var(--syntax-keyword)" },
  { tag: [tags.typeName, tags.standard(tags.name)], color: "var(--syntax-type)" },
  { tag: [tags.string, tags.special(tags.string)], color: "var(--syntax-string)" },
  { tag: [tags.number, tags.bool, tags.null], color: "var(--syntax-number)" },
  { tag: [tags.lineComment, tags.blockComment], color: "var(--syntax-comment)", fontStyle: "italic" },
]);
