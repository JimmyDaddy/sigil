#!/usr/bin/env ruby
# frozen_string_literal: true

REPO_ROOT = File.expand_path("..", __dir__)
en_files = Dir[File.join(REPO_ROOT, "docs", "en", "*.md")].map { |path| File.basename(path) }.sort
zh_files = Dir[File.join(REPO_ROOT, "docs", "zh-CN", "*.md")].map { |path| File.basename(path) }.sort

missing_zh = en_files - zh_files
missing_en = zh_files - en_files
errors = []
errors << "missing zh-CN docs: #{missing_zh.join(", ")}" unless missing_zh.empty?
errors << "missing en docs: #{missing_en.join(", ")}" unless missing_en.empty?

en_files.each do |file|
  next unless zh_files.include?(file)

  en_path = File.join(REPO_ROOT, "docs", "en", file)
  zh_path = File.join(REPO_ROOT, "docs", "zh-CN", file)
  en_text = File.read(en_path)
  zh_text = File.read(zh_path)
  errors << "docs/en/#{file} missing zh-CN language link" unless en_text.include?("../zh-CN/#{file}")
  errors << "docs/zh-CN/#{file} missing en language link" unless zh_text.include?("../en/#{file}")
end

if errors.empty?
  puts "docs mirror ok"
else
  warn errors.join("\n")
  exit 1
end
