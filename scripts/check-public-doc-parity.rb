#!/usr/bin/env ruby
# frozen_string_literal: true

require "json"

REPO_ROOT = File.expand_path("..", __dir__)
POLICY_PATH = File.join(REPO_ROOT, "dev", "docs", "public-documentation-content-policy.json")
POLICY = JSON.parse(File.read(POLICY_PATH))

def line_number(text, needle)
  index = text.each_line.to_a.index { |line| line.include?(needle) }
  index ? index + 1 : 1
end

def signature(markdown)
  {
    headings: markdown.scan(/^(\#{1,3})\s+/).flatten.map(&:length),
    fences: markdown.scan(/^```[ \t]*([^\s`]*)[ \t]*$/).flatten,
    images: markdown.scan(/!\[[^\]]*\]\(([^)]+)\)/).flatten.map { |path| File.basename(path.split("#", 2).first) },
    local_docs: markdown.scan(/\[[^\]]+\]\(([^)#]+\.md)(?:#[^)]*)?\)/).flatten.map { |path| File.basename(path) },
    topics: markdown.scan(/<!--\s*public-doc-topic:\s*([^\s]+)\s*-->/).flatten,
    ctas: markdown.scan(/<!--\s*public-doc-cta:\s*([^\s]+)\s*-->/).flatten,
    tables: markdown.each_line.count { |line| line.match?(/^\|.*\|\s*$/) },
    sections: section_signatures(markdown)
  }
end

def section_signatures(markdown)
  parts = markdown.split(/^##\s+.+?\s*$/)
  parts.drop(1).map do |body|
    {
      fences: body.scan(/^```[ \t]*([^\s`]*)[ \t]*$/).flatten,
      images: body.scan(/!\[[^\]]*\]\(([^)]+)\)/).flatten.map { |path| File.basename(path.split("#", 2).first) },
      local_docs: body.scan(/\[[^\]]+\]\(([^)#]+\.md)(?:#[^)]*)?\)/).flatten.map { |path| File.basename(path) },
      topics: body.scan(/<!--\s*public-doc-topic:\s*([^\s]+)\s*-->/).flatten,
      tables: body.each_line.count { |line| line.match?(/^\|.*\|\s*$/) },
      ordered_items: body.each_line.count { |line| line.match?(/^\d+\.\s+/) },
      unordered_items: body.each_line.count { |line| line.match?(/^[-*]\s+/) }
    }
  end
end

failures = []
POLICY.fetch("pages").each do |page|
  file = page.fetch("file")
  en_path = File.join(REPO_ROOT, "docs", "en", file)
  zh_path = File.join(REPO_ROOT, "docs", "zh-CN", file)
  next unless File.file?(en_path) && File.file?(zh_path)

  en = File.read(en_path)
  zh = File.read(zh_path)
  en_signature = signature(en)
  zh_signature = signature(zh)
  en_signature.each do |field, value|
    next if value == zh_signature.fetch(field)

    failures << "docs/en/#{file}:#{line_number(en, '##')}: [bilingual-#{field}] EN/ZH structure differs; authoritative source: #{page.fetch('slug')}"
    failures << "docs/zh-CN/#{file}:#{line_number(zh, '##')}: [bilingual-#{field}] EN/ZH structure differs; authoritative source: #{page.fetch('slug')}"
  end
end

if failures.empty?
  puts "public documentation bilingual parity checks passed"
else
  warn failures.join("\n")
  exit 1
end
