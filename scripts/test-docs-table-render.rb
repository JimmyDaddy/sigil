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
  "<tr><td>Open help</td><td><code>F1</code></td></tr>",
  "<tr><th>Command</th><th>Purpose</th></tr>",
  "<tr><td><code>/agent &lt;main|child-id&gt;</code></td><td>Switch the main chat area between the parent session and child agent transcripts</td></tr>",
  "<tr><td><code>/queue next|interrupt|edit|delete [item]</code></td><td>Keep a follow-up for the next turn, interrupt and run it now, edit it, or cancel it</td></tr>"
]

missing_rows = required_rows.reject { |row| rendered_reference.include?(row) }
unless missing_rows.empty?
  warn "rendered reference page is missing expected table rows:"
  missing_rows.each { |row| warn row }
  exit 1
end

puts "docs table render test passed"
