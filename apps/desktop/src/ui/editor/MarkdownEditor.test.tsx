import { createRef, useState } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MarkdownEditor, type MarkdownEditorHandle } from "./MarkdownEditor";
import { editorText, editorView, setEditorText } from "../../test/editor";

afterEach(cleanup);

function fixture(initial = "", onKeyDown = vi.fn(), onCompositionStart = vi.fn()) {
  const handle = createRef<MarkdownEditorHandle>();
  const input = createRef<HTMLElement>();
  function Harness({ expanded = false }: { expanded?: boolean }) {
    const [value, setValue] = useState(initial);
    return <MarkdownEditor ref={handle} value={value} label="Markdown source" inputRef={input}
      placeholder="Write" expanded={expanded} suggestionsOpen={false} onChange={setValue}
      onKeyDown={onKeyDown} onCompositionStart={onCompositionStart} onPaste={() => undefined} />;
  }
  const result = render(<Harness />);
  return { ...result, handle, input, Harness, editor: screen.getByRole("combobox") };
}

describe("Markdown editor transactions", () => {
  it("preserves CRLF, whitespace and Unicode as one source string", () => {
    const source = "  中文🙂\r\n    code  \n";
    const { editor } = fixture(source);
    expect(editorText(editor)).toBe(source);
    setEditorText(editor, `${source}tail`);
    expect(editorText(editor)).toBe(`${source}tail`);
  });
  it("undoes and redoes formatting with the same editor history and keyboard bindings", () => {
    const { editor, handle } = fixture("hello");
    const view = editorView(editor);
    act(() => view.dispatch({ selection: { anchor: 0, head: 5 } }));
    act(() => handle.current?.format("bold"));
    expect(editorText(editor)).toBe("**hello**");
    fireEvent.keyDown(editor, { key: "z", code: "KeyZ", ctrlKey: true });
    expect(editorText(editor)).toBe("hello");
    fireEvent.keyDown(editor, { key: "y", code: "KeyY", ctrlKey: true });
    expect(editorText(editor)).toBe("**hello**");
    setEditorText(editor, "**hello** next");
    act(() => handle.current?.undo());
    expect(editorText(editor)).toBe("**hello**");
  });
  it("keeps the editor, selection and history when expanded", () => {
    const { editor, handle, rerender, Harness } = fixture("中文");
    act(() => editorView(editor).dispatch({ selection: { anchor: 0, head: 2 } }));
    act(() => handle.current?.format("italic"));
    const before = editorView(editor);
    rerender(<Harness expanded />);
    expect(screen.getByRole("combobox")).toBe(editor);
    expect(editorView(editor)).toBe(before);
    expect(before.state.selection.main.from).toBe(1);
    act(() => handle.current?.undo());
    expect(editorText(editor)).toBe("中文");
  });
  it("reports composing before host suggestion/Enter logic and ignores format actions", () => {
    const onKeyDown = vi.fn();
    const onCompositionStart = vi.fn();
    const { editor, handle } = fixture("/", onKeyDown, onCompositionStart);
    fireEvent.compositionStart(editor);
    fireEvent.keyDown(editor, { key: "Enter", keyCode: 229, isComposing: true });
    expect(onKeyDown.mock.calls.at(-1)?.[1]).toBe(true);
    expect(onCompositionStart).toHaveBeenCalledOnce();
    act(() => handle.current?.format("bold"));
    expect(editorText(editor)).toBe("/");
    fireEvent.compositionEnd(editor);
  });
});
