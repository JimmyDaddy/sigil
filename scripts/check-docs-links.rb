#!/usr/bin/env ruby
# frozen_string_literal: true

REPO_ROOT = File.expand_path("..", __dir__)
paths = Dir[File.join(REPO_ROOT, "README*.md")] +
        Dir[File.join(REPO_ROOT, "docs", "{en,zh-CN}", "*.md")] +
        Dir[File.join(REPO_ROOT, "docs", "examples", "config", "*.md")]

missing = []
paths.each do |path|
  text = File.read(path)
  links = text.scan(/\[[^\]]*\]\(([^)]+)\)/).flatten +
          text.scan(/<img[^>]+src="([^"]+)"/).flatten +
          text.scan(/!\[[^\]]*\]\(([^)]+)\)/).flatten
  links.each do |href|
    target = href.split("#", 2).first
    next if target.empty? || target.start_with?("http://", "https://", "mailto:", "#")

    resolved = File.expand_path(target, File.dirname(path))
    missing << "#{path.sub("#{REPO_ROOT}/", "")}: #{href}" unless File.exist?(resolved)
  end
end

if missing.empty?
  puts "docs links ok"
else
  warn missing.join("\n")
  exit 1
end
