/**
 * The document itself, live.
 *
 * Because the editor mutates a `toml_edit` document rather than regenerating
 * one, this shows the actual file at all times — not an approximation of what
 * saving would produce. Editing here replaces the whole document, which costs
 * exactly one undo step however much changed.
 */
import { useEffect, useRef, useState } from "react";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Stack from "@mui/material/Stack";
import Typography from "@mui/material/Typography";
import { EditorState } from "@codemirror/state";
import { EditorView, keymap, lineNumbers, highlightActiveLine } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { StreamLanguage } from "@codemirror/language";
import { toml } from "@codemirror/legacy-modes/mode/toml";
import { useConfigDoc } from "../doc/ConfigDocProvider";

export function RawTomlPane() {
  const { text, replaceText } = useConfigDoc();
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const [draft, setDraft] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!host.current || view.current) return;
    const state = EditorState.create({
      doc: text,
      extensions: [
        lineNumbers(),
        highlightActiveLine(),
        history(),
        keymap.of([...defaultKeymap, ...historyKeymap]),
        StreamLanguage.define(toml),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) setDraft(u.state.doc.toString());
        }),
        EditorView.theme({
          "&": { fontSize: "13px" },
          ".cm-content": { fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace" },
          "&.cm-focused": { outline: "none" },
        }),
      ],
    });
    view.current = new EditorView({ state, parent: host.current });
    return () => {
      view.current?.destroy();
      view.current = null;
    };
    // Mounted once; external text changes are pushed in below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Push document changes made elsewhere (a slider, an undo) into the editor,
  // unless the user is mid-edit here.
  useEffect(() => {
    const v = view.current;
    if (!v || draft !== null) return;
    if (v.state.doc.toString() === text) return;
    v.dispatch({
      changes: { from: 0, to: v.state.doc.length, insert: text },
    });
  }, [text, draft]);

  const applyDraft = () => {
    if (draft === null) return;
    const failure = replaceText(draft);
    setError(failure);
    if (!failure) setDraft(null);
  };

  const discardDraft = () => {
    const v = view.current;
    if (v) {
      v.dispatch({ changes: { from: 0, to: v.state.doc.length, insert: text } });
    }
    setDraft(null);
    setError(null);
  };

  const dirty = draft !== null && draft !== text;

  return (
    <Stack spacing={1} sx={{ height: "100%" }}>
      <Stack direction="row" spacing={1} sx={{ alignItems: "center" }}>
        <Typography variant="body2" color="text.secondary" sx={{ flex: 1 }}>
          {dirty
            ? "Unapplied changes — the rest of the editor still shows the saved document."
            : "This is the live document, comments and all."}
        </Typography>
        <Button size="small" onClick={applyDraft} disabled={!dirty} variant="contained">
          Apply
        </Button>
        <Button size="small" onClick={discardDraft} disabled={!dirty}>
          Revert
        </Button>
      </Stack>

      {error && <Alert severity="error">{error}</Alert>}

      <Box
        ref={host}
        sx={{
          flex: 1,
          minHeight: 320,
          overflow: "auto",
          border: "1px solid",
          borderColor: dirty ? "warning.main" : "divider",
          borderRadius: 1,
        }}
      />
    </Stack>
  );
}
