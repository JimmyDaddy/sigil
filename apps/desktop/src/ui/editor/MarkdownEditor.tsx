import { Annotation, Compartment, EditorState, Prec, Transaction } from "@codemirror/state";
import { EditorView, keymap, placeholder } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, isolateHistory, redo, undo } from "@codemirror/commands";
import { forwardRef, useImperativeHandle, useLayoutEffect, useRef, type RefObject } from "react";

import { formatMarkdown, type MarkdownFormat } from "./markdownEditing";

export interface MarkdownEditorHandle {
  focus(): void;
  format(format: MarkdownFormat): void;
  undo(): void;
  redo(): void;
}

interface MarkdownEditorProps {
  readonly value: string;
  readonly label: string;
  readonly inputRef: RefObject<HTMLElement | null>;
  readonly placeholder: string;
  readonly expanded: boolean;
  readonly reviewActive?: boolean;
  readonly suggestionsOpen: boolean;
  readonly suggestionListId?: string;
  readonly activeSuggestionId?: string;
  readonly onChange: (value: string) => void;
  readonly onKeyDown: (event: KeyboardEvent, composing: boolean) => void;
  readonly onPaste: (event: ClipboardEvent) => void;
  readonly onCompositionStart: () => void;
}

const controlledReplacement = Annotation.define<boolean>();

// CodeMirror injects unlayered base styles. Its scoped theme adapter must override
// those defaults at the same boundary, while consuming only application roles.
const semanticTheme = EditorView.theme({
  "&": { minWidth: "0", minHeight: "48px", height: "100%", color: "var(--sg-sys-color-on-surface)", backgroundColor: "transparent" },
  "&.cm-focused": { outline: "1px solid var(--sg-sys-color-primary)", outlineOffset: "-1px" },
  ".cm-scroller": { overflow: "auto", fontFamily: "var(--sg-sys-font-mono)", fontSize: "14px", lineHeight: "1.6" },
  ".cm-content": { padding: "12px 10px", minHeight: "48px", caretColor: "var(--sg-sys-color-on-surface)" },
  ".cm-line": { padding: "0 4px" },
  ".cm-placeholder": { color: "var(--sg-sys-color-on-surface-subtle)" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--sg-sys-color-on-surface)" },
  ".cm-selectionBackground": { backgroundColor: "var(--sg-sys-color-primary-container)" },
  ".cm-content ::selection": { color: "var(--sg-sys-color-on-primary-container)", backgroundColor: "var(--sg-sys-color-primary-container)" },
});

export const MarkdownEditor = forwardRef<MarkdownEditorHandle, MarkdownEditorProps>(function MarkdownEditor(props, ref) {
  const host = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | undefined>(undefined);
  const current = useRef(props);
  current.current = props;
  const attributes = useRef(new Compartment());
  const hints = useRef(new Compartment());
  const composing = useRef(false);
  const applyFormat = (format: MarkdownFormat) => {
    const view = viewRef.current;
    if (view === undefined || composing.current || view.compositionStarted) return;
    const selection = view.state.selection.main;
    const edit = formatMarkdown(view.state.doc.toString(), selection.from, selection.to, format);
    view.dispatch({ changes: { from: edit.from, to: edit.to, insert: edit.insert },
      selection: { anchor: edit.anchor, head: edit.head },
      annotations: [Transaction.userEvent.of("input.format"), isolateHistory.of("full")] });
    view.focus();
  };
  useImperativeHandle(ref, () => ({
    focus: () => viewRef.current?.focus(), format: applyFormat,
    undo: () => { if (viewRef.current !== undefined) undo(viewRef.current); },
    redo: () => { if (viewRef.current !== undefined) redo(viewRef.current); },
  }));
  useLayoutEffect(() => {
    if (host.current === null) return;
    const view = new EditorView({ parent: host.current, state: EditorState.create({
      doc: current.current.value,
      extensions: [
        // Keep supplied carriage returns and Markdown whitespace in the source document.
        EditorState.lineSeparator.of("\n"), history(), EditorView.lineWrapping, semanticTheme,
        attributes.current.of(editorAttributes(current.current)),
        hints.current.of(placeholder(current.current.placeholder)),
        EditorView.updateListener.of((update) => {
          if (update.docChanged && !update.transactions.some((transaction) => transaction.annotation(controlledReplacement))) {
            current.current.onChange(update.state.doc.toString());
          }
        }),
        Prec.highest(EditorView.domEventHandlers({
          pointerdown(_event, editor) {
            if (document.activeElement !== editor.contentDOM) editor.contentDOM.focus({ preventScroll: true });
            return false;
          },
          keydown(event, editor) {
            current.current.onKeyDown(event, composing.current || editor.compositionStarted || event.isComposing || event.keyCode === 229);
            return event.defaultPrevented;
          },
          paste(event) { current.current.onPaste(event); return event.defaultPrevented; },
          compositionstart() { composing.current = true; current.current.onCompositionStart(); return false; },
          compositionend() { composing.current = false; return false; },
        })),
        keymap.of([
          { key: "Mod-b", run: () => { applyFormat("bold"); return true; } },
          { key: "Mod-i", run: () => { applyFormat("italic"); return true; } },
          { key: "Mod-k", run: () => { applyFormat("link"); return true; } },
          ...historyKeymap, ...defaultKeymap,
        ]),
      ],
    }) });
    viewRef.current = view;
    current.current.inputRef.current = view.contentDOM;
    return () => {
      if (current.current.inputRef.current === view.contentDOM) current.current.inputRef.current = null;
      viewRef.current = undefined;
      view.destroy();
    };
  }, []);
  useLayoutEffect(() => {
    const view = viewRef.current;
    if (view === undefined) return;
    view.dispatch({ effects: [attributes.current.reconfigure(editorAttributes(props)),
      hints.current.reconfigure(placeholder(props.placeholder))] });
    if (view.state.doc.toString() !== props.value) {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: props.value },
        annotations: [controlledReplacement.of(true), Transaction.userEvent.of("input.replace"), isolateHistory.of("full")] });
    }
    view.scrollDOM.style.maxHeight = props.reviewActive ? "88px" : props.expanded ? "100%" : "176px";
  }, [props.value, props.label, props.placeholder, props.expanded, props.reviewActive,
    props.suggestionsOpen, props.suggestionListId, props.activeSuggestionId]);
  return <div ref={host} className="sg-markdown-editor" data-expanded={props.expanded}
    onKeyDownCapture={(event) => {
      if (!(composing.current || viewRef.current?.compositionStarted || event.nativeEvent.isComposing || event.keyCode === 229)) return;
      // Let the browser commit its composition, without dispatching editor shortcuts or host actions.
      current.current.onKeyDown(event.nativeEvent, true);
      event.stopPropagation();
    }} />;
});

function editorAttributes(props: MarkdownEditorProps) {
  return EditorView.contentAttributes.of({ id: "desktop-prompt", tabindex: "0", role: "combobox", "aria-label": props.label,
    "aria-multiline": "true", "aria-autocomplete": "list", "aria-expanded": String(props.suggestionsOpen),
    ...(props.suggestionListId === undefined ? {} : { "aria-controls": props.suggestionListId }),
    ...(props.activeSuggestionId === undefined ? {} : { "aria-activedescendant": props.activeSuggestionId }),
    spellcheck: "false", autocapitalize: "off", autocomplete: "off", autocorrect: "off" });
}
