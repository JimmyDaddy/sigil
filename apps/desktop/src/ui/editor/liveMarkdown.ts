import { Facet, StateEffect, StateField, type EditorState, type Extension, type Range } from "@codemirror/state";
import { Decoration, EditorView, WidgetType, type DecorationSet } from "@codemirror/view";
import { syntaxTree } from "@codemirror/language";
import { markdown } from "@codemirror/lang-markdown";
import { GFM } from "@lezer/markdown";
import type { SyntaxNode } from "@lezer/common";

export const liveMarkdownEnabled = Facet.define<boolean, boolean>({ combine: (values) => values[0] ?? true });
export const markdownComposition = StateEffect.define<boolean>();

const interaction = StateField.define({
  create: () => false,
  update(value, transaction) {
    for (const effect of transaction.effects) {
      if (effect.is(markdownComposition)) value = effect.value;
    }
    return value;
  },
});

class ListBullet extends WidgetType {
  eq(): boolean { return true; }
  toDOM(): HTMLElement {
    const bullet = document.createElement("span");
    bullet.className = "cm-md-bullet";
    bullet.textContent = "•";
    bullet.setAttribute("aria-hidden", "true");
    return bullet;
  }
}

const headingClasses: Readonly<Record<string, string>> = {
  ATXHeading1: "cm-md-heading cm-md-h1", ATXHeading2: "cm-md-heading cm-md-h2",
  ATXHeading3: "cm-md-heading cm-md-h3", ATXHeading4: "cm-md-heading cm-md-h4",
  ATXHeading5: "cm-md-heading cm-md-h5", ATXHeading6: "cm-md-heading cm-md-h6",
  SetextHeading1: "cm-md-heading cm-md-h1", SetextHeading2: "cm-md-heading cm-md-h2",
};

function editingNode(state: EditorState, node: SyntaxNode): boolean {
  // Keep the selected syntax revealed while focus moves to editing controls too.
  // Replacing its DOM on blur would collapse the browser's selection.
  return state.selection.ranges.some((range) => range.empty
    ? range.from >= node.from && range.from < node.to
    : range.from < node.to && range.to > node.from);
}

function buildDecorations(state: EditorState): DecorationSet {
  if (!state.facet(liveMarkdownEnabled)) return Decoration.none;
  const ranges: Range<Decoration>[] = [];
  const replace = (from: number, to: number) => {
    if (to > from) ranges.push(Decoration.replace({}).range(from, to));
  };
  const mark = (node: SyntaxNode, className: string, tagName = "span") => {
    ranges.push(Decoration.mark({ class: className, tagName }).range(node.from, node.to));
  };
  const line = (position: number, className: string) => {
    ranges.push(Decoration.line({ class: className }).range(state.doc.lineAt(position).from));
  };
  const eachLine = (node: SyntaxNode, className: string) => {
    for (let number = state.doc.lineAt(node.from).number; number <= state.doc.lineAt(node.to).number; number += 1) {
      ranges.push(Decoration.line({ class: className }).range(state.doc.line(number).from));
    }
  };
  const hideChildMarks = (node: SyntaxNode, name: string) => {
    if (editingNode(state, node)) return;
    for (let child = node.firstChild; child !== null; child = child.nextSibling) {
      if (child.name === name) replace(child.from, child.to);
    }
  };
  syntaxTree(state).iterate({ enter({ node }) {
    const heading = headingClasses[node.name];
    if (heading !== undefined) {
      line(node.from, heading);
      if (!editingNode(state, node)) {
        for (let child = node.firstChild; child !== null; child = child.nextSibling) {
          if (child.name !== "HeaderMark") continue;
          let end = child.to;
          if (child.from === node.from) {
            while (end < node.to && (state.doc.sliceString(end, end + 1) === " " || state.doc.sliceString(end, end + 1) === "\t")) end += 1;
          }
          replace(child.from, end);
        }
      }
    }
    switch (node.name) {
      case "StrongEmphasis": mark(node, "cm-md-strong", "strong"); hideChildMarks(node, "EmphasisMark"); break;
      case "Emphasis": mark(node, "cm-md-emphasis", "em"); hideChildMarks(node, "EmphasisMark"); break;
      case "Strikethrough": mark(node, "cm-md-strike", "s"); hideChildMarks(node, "StrikethroughMark"); break;
      case "InlineCode": mark(node, "cm-md-inline-code", "code"); hideChildMarks(node, "CodeMark"); return false;
      case "FencedCode": case "CodeBlock": eachLine(node, "cm-md-code-line"); return false;
      case "Blockquote": eachLine(node, "cm-md-quote-line"); break;
      case "QuoteMark": {
        const currentLine = state.doc.lineAt(node.from);
        const editingLine = state.selection.ranges.some((range) =>
          range.from <= currentLine.to && range.to >= currentLine.from);
        if (!editingLine) replace(node.from, node.to);
        break;
      }
      case "ListItem": eachLine(node, "cm-md-list-line"); break;
      case "ListMark":
        if (node.parent?.parent?.name === "BulletList" && !editingNode(state, node.parent)) {
          ranges.push(Decoration.replace({ widget: new ListBullet() }).range(node.from, node.to));
        }
        break;
      case "Link": {
        mark(node, "cm-md-link");
        if (!editingNode(state, node)) {
          let close: SyntaxNode | undefined;
          for (let child = node.firstChild; child !== null; child = child.nextSibling) {
            if (child.name === "LinkMark" && state.doc.sliceString(child.from, child.to) === "[") replace(child.from, child.to);
            else if (child.name === "LinkMark" && state.doc.sliceString(child.from, child.to) === "]") close = child;
          }
          if (close !== undefined) replace(close.from, node.to);
        }
        break;
      }
      case "Autolink": mark(node, "cm-md-link"); hideChildMarks(node, "LinkMark"); break;
      // Images and HTML stay inert source; never introduce fetchable or executable DOM.
      case "Image": case "HTMLBlock": case "InlineHTML": return false;
    }
  } });
  return Decoration.set(ranges, true);
}

export const liveMarkdownDecorations = StateField.define<DecorationSet>({
  create: buildDecorations,
  update(value, transaction) {
    if (transaction.state.field(interaction)) return value.map(transaction.changes);
    return buildDecorations(transaction.state);
  },
  provide: (field) => EditorView.decorations.from(field),
});

export const liveMarkdown: Extension = [
  markdown({ extensions: GFM, addKeymap: false, completeHTMLTags: false, pasteURLAsLink: false }),
  interaction, liveMarkdownDecorations,
];
