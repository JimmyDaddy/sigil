import { Annotation, Compartment, EditorState, Prec, Transaction } from "@codemirror/state";
import { EditorView, keymap, placeholder } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, isolateHistory, redo, undo } from "@codemirror/commands";
import { forwardRef, useImperativeHandle, useLayoutEffect, useRef, type RefObject } from "react";

import { formatMarkdown, type MarkdownFormat } from "./markdownEditing";
import { liveMarkdown, liveMarkdownEnabled, markdownComposition } from "./liveMarkdown";

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
  readonly sourceMode?: boolean;
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
  "&": { minWidth: "0", minHeight: "48px", height: "auto", color: "var(--sg-sys-color-on-surface)", backgroundColor: "transparent" },
  "&.cm-focused": { outline: "1px solid var(--sg-sys-color-primary)", outlineOffset: "-1px" },
  ".cm-scroller": { overflow: "auto", fontFamily: "inherit", fontSize: "var(--sg-sys-type-body-size)", lineHeight: "1.6" },
  ".cm-content": { padding: "12px 10px", minHeight: "48px", caretColor: "var(--sg-sys-color-on-surface)" },
  ".cm-line": { padding: "0 4px" },
  ".cm-placeholder": { color: "var(--sg-sys-color-on-surface-subtle)" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--sg-sys-color-on-surface)" },
  ".cm-selectionBackground": { backgroundColor: "var(--sg-sys-color-primary-container)" },
  "&.cm-source-mode .cm-scroller": { fontFamily: "var(--sg-sys-font-mono)", fontSize: "14px" },
  ".cm-md-strong": { fontWeight: "700" },
  ".cm-md-emphasis": { fontStyle: "italic" },
  ".cm-md-strike": { textDecoration: "line-through" },
  ".cm-md-heading": { fontWeight: "700", lineHeight: "1.4", paddingTop: "5px", paddingBottom: "3px" },
  ".cm-md-h1": { fontSize: "19px" }, ".cm-md-h2": { fontSize: "17px" },
  ".cm-md-h3": { fontSize: "16px" },
  ".cm-md-h4, .cm-md-h5, .cm-md-h6": { fontSize: "15px" },
  ".cm-md-inline-code": { padding: "1px 3px", borderRadius: "3px", fontFamily: "var(--sg-sys-font-mono)", fontSize: ".92em", backgroundColor: "var(--sg-sys-color-surface-container-high)" },
  ".cm-md-code-line": { fontFamily: "var(--sg-sys-font-mono)", fontSize: "13px", backgroundColor: "var(--sg-sys-color-surface-container-high)" },
  ".cm-md-link": { color: "var(--sg-sys-color-primary)", textDecoration: "underline", textUnderlineOffset: "2px" },
  ".cm-md-quote-line": { paddingLeft: "10px", borderLeft: "3px solid var(--sg-sys-color-outline-variant)", color: "var(--sg-sys-color-on-surface-variant)" },
  ".cm-md-list-line": { paddingLeft: "7px" },
  ".cm-md-bullet": { color: "var(--sg-sys-color-on-surface-variant)", paddingRight: "2px" },
  ".cm-content ::selection": { color: "var(--sg-sys-color-on-primary-container)", backgroundColor: "var(--sg-sys-color-primary-container)" },
});

export const MarkdownEditor = forwardRef<MarkdownEditorHandle, MarkdownEditorProps>(function MarkdownEditor(props, ref) {
  const host = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | undefined>(undefined);
  const current = useRef(props);
  current.current = props;
  const attributes = useRef(new Compartment());
  const hints = useRef(new Compartment());
  const presentation = useRef(new Compartment());
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
      selection: { anchor: current.current.value.length },
      extensions: [
        // Keep supplied carriage returns and Markdown whitespace in the source document.
        EditorState.lineSeparator.of("\n"), history(), EditorView.lineWrapping, semanticTheme,
        liveMarkdown, presentation.current.of(presentationExtensions(current.current.sourceMode)),
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
          compositionstart(_event, editor) {
            composing.current = true;
            current.current.onCompositionStart();
            editor.dispatch({ effects: markdownComposition.of(true) });
            return false;
          },
          compositionend(_event, editor) {
            composing.current = false;
            // Let the browser and CodeMirror flush the committed composition first.
            window.setTimeout(() => {
              if (viewRef.current === editor && !composing.current && !editor.compositionStarted) {
                editor.dispatch({ effects: markdownComposition.of(false) });
              }
            }, 0);
            return false;
          },
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
      hints.current.reconfigure(placeholder(props.placeholder)),
      presentation.current.reconfigure(presentationExtensions(props.sourceMode))] });
    if (view.state.doc.toString() !== props.value) {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: props.value },
        annotations: [controlledReplacement.of(true), Transaction.userEvent.of("input.replace"), isolateHistory.of("full")] });
    }
    view.scrollDOM.style.maxHeight = props.reviewActive ? "88px" : props.expanded ? "min(280px, 38dvh)" : "min(176px, 24dvh)";
  }, [props.value, props.label, props.placeholder, props.expanded, props.reviewActive,
    props.suggestionsOpen, props.suggestionListId, props.activeSuggestionId, props.sourceMode]);
  return <div ref={host} className="sg-markdown-editor" data-expanded={props.expanded}
    onKeyDownCapture={(event) => {
      if (!(composing.current || viewRef.current?.compositionStarted || event.nativeEvent.isComposing || event.keyCode === 229)) return;
      // Let the browser commit its composition, without dispatching editor shortcuts or host actions.
      current.current.onKeyDown(event.nativeEvent, true);
      event.stopPropagation();
    }} />;
});

function presentationExtensions(sourceMode = false) {
  return [liveMarkdownEnabled.of(!sourceMode), EditorView.editorAttributes.of({ class: sourceMode ? "cm-source-mode" : "" })];
}

function editorAttributes(props: MarkdownEditorProps) {
  return EditorView.contentAttributes.of({ id: "desktop-prompt", tabindex: "0", role: "combobox", "aria-label": props.label,
    "aria-multiline": "true", "aria-autocomplete": "list", "aria-expanded": String(props.suggestionsOpen),
    ...(props.suggestionListId === undefined ? {} : { "aria-controls": props.suggestionListId }),
    ...(props.activeSuggestionId === undefined ? {} : { "aria-activedescendant": props.activeSuggestionId }),
    spellcheck: "false", autocapitalize: "off", autocomplete: "off", autocorrect: "off" });
}
