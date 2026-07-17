#!/usr/bin/env ruby
# frozen_string_literal: true

require "fileutils"
require "json"
require "tmpdir"
require_relative "check-public-doc-content"

SOURCE_ROOT = File.expand_path("..", __dir__)
POLICY = JSON.parse(File.read(File.join(SOURCE_ROOT, PublicDocContent::POLICY_PATH)))

FIXTURE_PATHS = begin
  pages = POLICY.fetch("locale_roots").values.flat_map do |directory|
    POLICY.fetch("pages").map { |page| "#{directory}/#{page.fetch('file')}" }
  end
  roots = POLICY.fetch("root_readmes").map { |entry| entry.fetch("path") }
  (pages + roots + POLICY.fetch("entry_html") + POLICY.fetch("config_examples") +
    [PublicDocContent::POLICY_PATH, PublicDocContent::GENERATOR_PATH]).uniq.freeze
end

def copy_fixture(root)
  FIXTURE_PATHS.each do |path|
    source = File.join(SOURCE_ROOT, path)
    target = File.join(root, path)
    FileUtils.mkdir_p(File.dirname(target))
    FileUtils.cp(source, target)
  end
end

def rewrite(root, path)
  target = File.join(root, path)
  source = File.read(target)
  updated = yield(source)
  raise "mutation for #{path} made no change" if updated == source

  File.write(target, updated)
end

def assert_clean(root, label)
  violations = PublicDocContent.run(root: root)
  return if violations.empty?

  raise "#{label}: expected no violations\n#{violations.map(&:to_s).join("\n")}"
end

def assert_rule(label, rule)
  Dir.mktmpdir("sigil-public-doc-content-") do |root|
    copy_fixture(root)
    yield(root)
    violations = PublicDocContent.run(root: root)
    match = violations.find { |violation| violation.rule == rule }
    unless match
      raise "#{label}: expected [#{rule}], got\n#{violations.map(&:to_s).join("\n")}"
    end
    unless match.to_s.match?(/\A[^:]+(?:\/[^:]+)*:\d+: \[[A-Za-z0-9-]+\] .+; authoritative source: .+/)
      raise "#{label}: violation format is unstable: #{match}"
    end
  end
end

def assert_clean_case(label)
  Dir.mktmpdir("sigil-public-doc-content-clean-case-") do |root|
    copy_fixture(root)
    yield(root)
    assert_clean(root, label)
  end
end

Dir.mktmpdir("sigil-public-doc-content-clean-") do |root|
  copy_fixture(root)
  assert_clean(root, "production fixture")
end

assert_rule("internal implementation language", "implementation-language") do |root|
  rewrite(root, "docs/en/status.md") { |text| text + "\nA provider crate requires exact proof.\n" }
end

assert_rule("bare implementation boundary terms", "implementation-language") do |root|
  rewrite(root, "docs/en/status.md") { |text| text + "\nThe adapter uses CAS and projection.\n" }
end

assert_rule("crate frontier and receipt implementation terms", "implementation-language") do |root|
  rewrite(root, "docs/en/status.md") do |text|
    text + "\nThis crate uses a frontier and receipt implementation.\n"
  end
end

assert_rule("plural receipt implementation term", "implementation-language") do |root|
  rewrite(root, "docs/en/status.md") { |text| text + "\nReceipt implementations are internal.\n" }
end

assert_rule("hyphenated frozen request term", "implementation-language") do |root|
  rewrite(root, "docs/en/status.md") { |text| text + "\nA frozen-request is retained.\n" }
end

assert_rule("eval fixture term", "implementation-language") do |root|
  rewrite(root, "docs/en/status.md") { |text| text + "\nAn eval fixture covers this path.\n" }
end

assert_rule("module responsibility term", "implementation-language") do |root|
  rewrite(root, "docs/en/status.md") { |text| text + "\nThis is a module responsibility.\n" }
end

assert_rule("hyphenated receipt implementation term", "implementation-language") do |root|
  rewrite(root, "docs/en/status.md") { |text| text + "\nA receipt-implementation is internal.\n" }
end

assert_rule("hyphenated module responsibility term", "implementation-language") do |root|
  rewrite(root, "docs/en/status.md") { |text| text + "\nThis is a module-responsibility.\n" }
end

assert_rule("config example implementation language", "implementation-language") do |root|
  rewrite(root, "docs/examples/config/mcp-safe-defaults.toml") do |text|
    text + "\n# The provider crate uses an exact proof.\n"
  end
end

assert_rule("MCP config example requires transport", "config-example") do |root|
  rewrite(root, "docs/examples/config/mcp-safe-defaults.toml") do |text|
    text.sub(%(transport = "stdio"\n), "")
  end
end

assert_rule("HTML prose after script", "implementation-language") do |root|
  rewrite(root, "site/index.html") do |text|
    text.sub("</body>", "<p>A provider crate requires exact proof.</p>\n</body>")
  end
end

assert_clean_case("implementation terms inside a Markdown fence are ignored") do |root|
  rewrite(root, "docs/en/status.md") do |text|
    text.sub("\n<!-- public-doc-cta:", "\n```text\nprovider crate exact proof\n```\n\n<!-- public-doc-cta:")
  end
end

assert_clean_case("implementation terms inside an HTML comment are ignored") do |root|
  rewrite(root, "site/index.html") do |text|
    text.sub("</body>", "<!-- provider crate exact proof -->\n</body>")
  end
end

assert_rule("ninth docs hub route", "hub-task-routes") do |root|
  rewrite(root, "site/docs/index.html") do |text|
    needle = %(          <a class="resource-card task-card" id="reference" href="reference/" data-step="08">)
    insertion = <<~HTML.chomp
          <a class="resource-card task-card" id="extra" href="extra/" data-step="09">
            <h3>Extra route</h3>
          </a>
    HTML
    text.sub(needle, insertion + "\n" + needle)
  end
end

assert_rule("version outside authority", "release-version") do |root|
  rewrite(root, "docs/en/status.md") { |text| text + "\nThe current package is v9.9.9-preview.1.\n" }
end

assert_rule("version without v prefix outside authority", "release-version") do |root|
  rewrite(root, "docs/en/status.md") { |text| text + "\nThe current package is 9.9.9-preview.1.\n" }
end

assert_rule("tag text does not exempt version outside authority", "release-version") do |root|
  rewrite(root, "docs/en/status.md") do |text|
    text + "\nUse --tag when referring to version 9.9.9-preview.1.\n"
  end
end

assert_rule("missing locale page", "locale-pair") do |root|
  File.delete(File.join(root, "docs/zh-CN/cookbook.md"))
end

assert_rule("page role drift", "page-role") do |root|
  rewrite(root, "docs/en/quickstart.md") do |text|
    text.sub("authority: first-success", "authority: task-router")
  end
end

assert_rule("policy role drift", "page-role") do |root|
  rewrite(root, PublicDocContent::POLICY_PATH) do |text|
    text.sub(%("role": "first-success"), %("role": "first-success-drift"))
  end
end

assert_rule("published slug inventory cannot drift", "policy-inventory") do |root|
  rewrite(root, PublicDocContent::POLICY_PATH) do |text|
    text.sub(%("slug": "cookbook"), %("slug": "cookbook-extra"))
  end
end

assert_rule("root README contract cannot shrink", "policy-contract") do |root|
  rewrite(root, PublicDocContent::POLICY_PATH) do |text|
    policy = JSON.parse(text)
    policy.fetch("root_readmes").first.delete("role")
    JSON.pretty_generate(policy) + "\n"
  end
end

assert_rule("topic authority contract cannot shrink", "policy-contract") do |root|
  rewrite(root, PublicDocContent::POLICY_PATH) do |text|
    policy = JSON.parse(text)
    policy.delete("topic_authorities")
    JSON.pretty_generate(policy) + "\n"
  end
end

assert_rule("policy heading key drift", "page-role") do |root|
  rewrite(root, PublicDocContent::POLICY_PATH) do |text|
    text.sub(%("before-you-begin",), %("garbage-heading-key",))
  end
end

assert_rule("policy CTA key drift", "page-role") do |root|
  rewrite(root, PublicDocContent::POLICY_PATH) do |text|
    text.sub(%("cta_key": "continue-by-task"), %("cta_key": "garbage-cta"))
  end
end

assert_rule("policy cannot remove required safety topics", "policy-contract") do |root|
  rewrite(root, PublicDocContent::POLICY_PATH) do |text|
    policy = JSON.parse(text)
    policy.fetch("pages").find { |page| page.fetch("slug") == "safety" }.delete("required_topics")
    JSON.pretty_generate(policy) + "\n"
  end
end

assert_rule("policy cannot remove search authority cases", "policy-contract") do |root|
  rewrite(root, PublicDocContent::POLICY_PATH) do |text|
    policy = JSON.parse(text)
    policy["search_first_result"] = { "en" => {}, "zh-CN" => {} }
    JSON.pretty_generate(policy) + "\n"
  end
end

assert_rule("localized heading drift", "page-headings") do |root|
  rewrite(root, "docs/en/safety.md") do |text|
    text.sub("## Risk Model", "## Unrelated Heading")
  end
end

assert_rule("required CTA removed", "page-cta") do |root|
  rewrite(root, "docs/en/cookbook.md") do |text|
    text.sub("Next: [Choose the matching workflow](workflows.md).", "Next: Choose a workflow.")
  end
end

assert_rule("required CTA label drift", "page-cta") do |root|
  rewrite(root, "docs/en/cookbook.md") do |text|
    text.sub("[Choose the matching workflow]", "[Browse elsewhere]")
  end
end

assert_rule("generator page drift", "generator-pages") do |root|
  rewrite(root, "scripts/build-docs-site.rb") do |text|
    text.sub(%(["cookbook", "cookbook.md", "Cookbook"],), %(["cookbook-extra", "cookbook.md", "Cookbook"],))
  end
end

assert_rule("required safety topic deletion", "required-topic") do |root|
  rewrite(root, "docs/en/safety.md") do |text|
    text.sub("<!-- public-doc-topic: approval-risk-model -->", "")
  end
end

assert_rule("required safety topic body deletion", "required-topic") do |root|
  rewrite(root, "docs/en/safety.md") do |text|
    text.sub(/(<!-- public-doc-topic: approval-risk-model -->\n).*?(?=\n## Review An Approval)/m, "\\1")
  end
end

assert_rule("fixed public inventory cannot shrink through policy", "policy-inventory") do |root|
  rewrite(root, PublicDocContent::POLICY_PATH) do |text|
    text.sub(%("site/404.html"), %("site/index.html"))
  end
end

assert_rule("duplicate installation channel", "installation-authority") do |root|
  rewrite(root, "docs/en/quickstart.md") do |text|
    text + "\n```bash\nbrew install JimmyDaddy/sigil/sigil-ai\n```\n"
  end
end

assert_rule("duplicate shortcut matrix", "key-matrix-authority") do |root|
  rewrite(root, "docs/en/user-guide.md") do |text|
    text + <<~MARKDOWN

      | Action | Key |
      | --- | --- |
      | One | `Ctrl-A` |
      | Two | `Ctrl-B` |
      | Three | `Alt-C` |
      | Four | `F4` |
    MARKDOWN
  end
end

assert_rule("shortcut matrix does not depend on header wording", "key-matrix-authority") do |root|
  rewrite(root, "docs/en/user-guide.md") do |text|
    text + <<~MARKDOWN

      | Action | Shortcut |
      | --- | --- |
      | One | `Ctrl-A` |
      | Two | `Ctrl-B` |
      | Three | `Alt-C` |
      | Four | `F4` |
    MARKDOWN
  end
end

assert_rule("shortcut matrix recognizes ordinary key names", "key-matrix-authority") do |root|
  rewrite(root, "docs/en/user-guide.md") do |text|
    text + <<~MARKDOWN

      | Action | Shortcut |
      | --- | --- |
      | One | `Enter` |
      | Two | `Esc` |
      | Three | `Tab` |
      | Four | `Up/Down` |
    MARKDOWN
  end
end

assert_rule("provider auth outside authority", "provider-auth-authority") do |root|
  rewrite(root, "docs/en/status.md") { |text| text + "\nSet SIGIL_ANTHROPIC_API_KEY before launch.\n" }
end

assert_rule("README prose budget", "readme-budget") do |root|
  rewrite(root, "README.md") { |text| text + "\n" + (["overflow"] * 950).join(" ") + "\n" }
end

assert_rule("README primary list survives heading rename", "readme-primary-items") do |root|
  rewrite(root, "README.md") do |text|
    extra = (1..9).map { |index| "- Extra value #{index}" }.join("\n")
    text.sub("## Why Sigil", "## Product Value") + "\n## Extra Value\n\n#{extra}\n"
  end
end

assert_rule("README primary items cannot split across lists", "readme-primary-items") do |root|
  rewrite(root, "README.md") do |text|
    first = (1..5).map { |index| "- Value A#{index}" }.join("\n")
    second = (1..5).map { |index| "- Value B#{index}" }.join("\n")
    text + "\n## More Value\n\n#{first}\n\nParagraph.\n\n#{second}\n"
  end
end

assert_rule("README primary items include CommonMark plus lists", "readme-primary-items") do |root|
  rewrite(root, "README.md") do |text|
    extra = (1..10).map { |index| "  + Extra value #{index}" }.join("\n")
    text + "\n## More Value\n\n#{extra}\n"
  end
end

assert_rule("homepage feature budget", "homepage-feature-budget") do |root|
  rewrite(root, "site/index.html") do |text|
    text.sub(%(<div class="feature-grid">), %(<div class="feature-grid"><article><h3>Extra</h3></article>))
  end
end

assert_rule("nested homepage feature budget", "homepage-feature-budget") do |root|
  rewrite(root, "site/index.html") do |text|
    insertion = %(<article><div><h3>Nested extra</h3></div></article>)
    text.sub(%(<div class="feature-grid">), %(<div class="feature-grid">#{insertion}))
  end
end

assert_rule("single-quoted homepage feature budget", "homepage-feature-budget") do |root|
  rewrite(root, "site/index.html") do |text|
    text.sub(%(<div class="feature-grid">), %(<div class='feature-grid'><article><h3>Extra</h3></article>))
  end
end

assert_rule("allowlisted command cannot duplicate", "installation-authority") do |root|
  rewrite(root, "README.md") do |text|
    text + "\n```bash\nnpm install -g @sigil-ai/sigil@alpha\n```\n"
  end
end

puts "public documentation content negative tests passed"
