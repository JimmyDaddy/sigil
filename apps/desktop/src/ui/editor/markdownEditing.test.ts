import { describe, expect, it } from "vitest";
import { formatMarkdown } from "./markdownEditing";

const apply = (source: string, from: number, to: number, format: Parameters<typeof formatMarkdown>[3]) => {
  const edit = formatMarkdown(source, from, to, format);
  return { text: source.slice(0, edit.from) + edit.insert + source.slice(edit.to), edit };
};

describe("Markdown source formatting", () => {
  it("preserves Unicode and surrounding Markdown whitespace", () => {
    const source = "  中文🙂  \nnext";
    const { text, edit } = apply(source, 2, 6, "bold");
    expect(text).toBe("  **中文🙂**  \nnext");
    expect(text.slice(edit.anchor, edit.head)).toBe("中文🙂");
    expect(apply("**中文**", 0, 6, "bold").text).toBe("中文");
  });
  it("uses safe code delimiters without rewriting the selected source", () => {
    expect(apply("`x`", 0, 3, "code").text).toBe("`` `x` ``");
    expect(apply("a```b", 0, 5, "code_block").text).toBe("````\na```b\n````");
    expect(apply("prefix tail", 7, 7, "code_block").text).toBe("prefix \n```\n\n```\ntail");
  });
  it("formats exactly selected lines, including a selection ending at the next line", () => {
    expect(apply("one\ntwo\nthree", 0, 4, "list").text).toBe("- one\ntwo\nthree");
    expect(apply("\none", 0, 0, "list").text).toBe("- \none");
    expect(apply("- one\n- two", 0, 11, "list").text).toBe("one\ntwo");
  });
  it("selects the URL placeholder while retaining the exact link label", () => {
    const { text, edit } = apply("中文", 0, 2, "link");
    expect(text).toBe("[中文](https://)");
    expect(text.slice(edit.anchor, edit.head)).toBe("https://");
  });
});
