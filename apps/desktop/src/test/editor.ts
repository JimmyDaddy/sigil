import { Transaction } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { act } from "@testing-library/react";

export function editorView(element: HTMLElement): EditorView {
  const view = EditorView.findFromDOM(element);
  if (view === null) throw new Error("expected the mounted Markdown editor");
  return view;
}

export function editorText(element: HTMLElement): string {
  return editorView(element).state.doc.toString();
}

export function setEditorText(element: HTMLElement, value: string): void {
  const view = editorView(element);
  act(() => view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: value },
    selection: { anchor: value.length }, annotations: Transaction.userEvent.of("input.type") }));
}

export function typeEditorText(element: HTMLElement, value: string): void {
  const view = editorView(element);
  element.focus({ preventScroll: true });
  const from = view.state.doc.length;
  act(() => view.dispatch({ changes: { from, insert: value }, selection: { anchor: from + value.length },
    annotations: Transaction.userEvent.of("input.type") }));
}
