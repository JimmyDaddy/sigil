#!/usr/bin/env ruby
# frozen_string_literal: true

REPO_ROOT = File.expand_path("..", __dir__)
commands_rs = File.read(File.join(REPO_ROOT, "crates", "sigil-tui", "src", "commands.rs"))
application_catalog_rs = File.read(
  File.join(REPO_ROOT, "crates", "sigil-runtime", "src", "application_catalog.rs")
)
reference_en = File.read(File.join(REPO_ROOT, "docs", "en", "reference.md"))
reference_zh = File.read(File.join(REPO_ROOT, "docs", "zh-CN", "reference.md"))

keys = commands_rs.scan(/KeyBinding\s*\{\s*label:\s*"([^"]+)"/).flatten.uniq
slash_commands = application_catalog_rs.scan(/canonical:\s*"([^"]+)"/).flatten.uniq
slash_aliases = application_catalog_rs.scan(/aliases:\s*&\[(.*?)\]/m).flat_map do |match|
  match.first.scan(/"([^"]+)"/).flatten
end.uniq
valid_slash_commands = slash_commands + slash_aliases

errors = []
errors << "shared application catalog exposes no slash commands" if slash_commands.empty?
(keys - ["Enter"]).each do |key|
  errors << "docs/en/reference.md missing key #{key}" unless reference_en.include?(key)
  errors << "docs/zh-CN/reference.md missing key #{key}" unless reference_zh.include?(key)
end

slash_commands.each do |command|
  errors << "docs/en/reference.md missing slash command #{command}" unless reference_en.include?(command)
  errors << "docs/zh-CN/reference.md missing slash command #{command}" unless reference_zh.include?(command)
end

[["docs/en/reference.md", reference_en], ["docs/zh-CN/reference.md", reference_zh]].each do |path, text|
  documented = text.scan(/`(\/[a-z][^`]*)`/).flatten.map do |entry|
    token = entry.split(/\s+/, 2).first
    token == "/plan" ? "/plan" : token
  end.uniq
  documented.each do |command|
    next if valid_slash_commands.include?(command)

    errors << "#{path} documents unknown slash command #{command}"
  end
end

if errors.empty?
  puts "docs command metadata ok"
else
  warn errors.join("\n")
  exit 1
end
