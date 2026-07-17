#!/usr/bin/env ruby
# frozen_string_literal: true

require_relative "build-docs-site"

FIXTURE_DIR = File.join(__dir__, "fixtures")
fixture = File.read(File.join(FIXTURE_DIR, "docs-table.md"))
expected = File.read(File.join(FIXTURE_DIR, "docs-table.golden.html")).strip
actual, = render_markdown(fixture, "en")

unless actual.strip == expected
  warn "docs table render did not match the golden output"
  warn "expected:\n#{expected}"
  warn "actual:\n#{actual.strip}"
  exit 1
end

reference = File.read(File.join(REPO_ROOT, "docs", "en", "reference.md"))
rendered_reference, = render_markdown(reference, "en")
required_rows = [
  "<tr><th>Action</th><th>Key</th></tr>",
  "<tr><td>Open help / slash selector</td><td><code>F1</code> / <code>/</code></td></tr>",
  "<tr><th>Command</th><th>Purpose</th></tr>",
  "<tr><td><code>/agent &lt;main|child-id&gt;</code></td><td>Switch visible transcript</td></tr>",
  "<tr><td><code>/queue next|interrupt|edit|delete [item]</code></td><td>Reorder, interrupt for, edit, or remove a follow-up</td></tr>"
]

missing_rows = required_rows.reject { |row| rendered_reference.include?(row) }
unless missing_rows.empty?
  warn "rendered reference page is missing expected table rows:"
  missing_rows.each { |row| warn row }
  exit 1
end

escaping_fixture = <<~MARKDOWN
  ![A & "B"](https://example.com/image.svg?mode=a&view=b)

  [Search & filter](https://example.com/docs?q=a&lang=en)

  [`dev/docs`](https://example.com/dev/docs)
MARKDOWN
rendered_escaping, = render_markdown(escaping_fixture, "en")
required_escaping = [
  '<img src="https://example.com/image.svg?mode=a&amp;view=b" alt="A &amp; &quot;B&quot;" loading="lazy" decoding="async">',
  '<a href="https://example.com/docs?q=a&amp;lang=en">Search &amp; filter</a>',
  '<a href="https://example.com/dev/docs"><code>dev/docs</code></a>'
]

missing_escaping = required_escaping.reject { |fragment| rendered_escaping.include?(fragment) }
unless missing_escaping.empty?
  warn "docs inline rendering is missing expected escaped output:"
  missing_escaping.each { |fragment| warn fragment }
  warn "actual:\n#{rendered_escaping}"
  exit 1
end

if rendered_escaping.include?("&amp;amp;")
  warn "docs inline rendering double-escaped an HTML entity"
  exit 1
end

comment_fixture = <<~MARKDOWN
  <!-- public-doc-role: fixture -->

  # Visible title

  <!--
  hidden implementation note
  -->

  Visible prose.
MARKDOWN
rendered_comment, = render_markdown(comment_fixture, "en")
if rendered_comment.include?("public-doc-role") || rendered_comment.include?("hidden implementation note")
  warn "docs renderer leaked an HTML comment into public output"
  exit 1
end
unless rendered_comment.include?("Visible title") && rendered_comment.include?("Visible prose.")
  warn "docs renderer removed visible prose while stripping comments"
  exit 1
end

search_comment = plain_text_from_markdown(comment_fixture)
if search_comment.include?("public-doc-role") || search_comment.include?("hidden implementation note")
  warn "docs search text leaked an HTML comment"
  exit 1
end

puts "docs table render test passed"
