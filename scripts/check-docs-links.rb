#!/usr/bin/env ruby
# frozen_string_literal: true

require "uri"

require_relative "build-docs-site"

paths = %w[README.md README.zh-CN.md AGENTS.md CONTRIBUTING.md SECURITY.md]
        .map { |path| File.join(REPO_ROOT, path) }
        .select { |path| File.file?(path) } +
        Dir[File.join(REPO_ROOT, "docs", "{en,zh-CN}", "*.md")] +
        Dir[File.join(REPO_ROOT, "docs", "examples", "config", "*.md")] +
        Dir[File.join(REPO_ROOT, "dev", "{docs,governance}", "**", "*.md")]

missing = []
heading_ids_by_path = {}

def heading_ids_for(path)
  heading_counts = Hash.new(0)
  File.readlines(path, chomp: true).filter_map do |line|
    next unless (match = line.match(/^(#+)\s+(.+)$/))

    unique_heading_id(match[2], heading_counts)
  end
end

paths.each do |path|
  text = File.read(path)
  links = text.scan(/\[[^\]]*\]\(([^)]+)\)/).flatten +
          text.scan(/<img[^>]+src="([^"]+)"/).flatten +
          text.scan(/!\[[^\]]*\]\(([^)]+)\)/).flatten
  links.each do |href|
    target, fragment = href.split("#", 2)
    next if target.start_with?("http://", "https://", "mailto:")

    resolved = target.empty? ? path : File.expand_path(target, File.dirname(path))
    unless File.exist?(resolved)
      missing << "#{path.sub("#{REPO_ROOT}/", "")}: #{href}"
      next
    end

    next if fragment.nil? || fragment.empty? || File.extname(resolved) != ".md"

    expected_id = URI::DEFAULT_PARSER.unescape(fragment)
    heading_ids_by_path[resolved] ||= heading_ids_for(resolved)
    next if heading_ids_by_path.fetch(resolved).include?(expected_id)

    missing << "#{path.sub("#{REPO_ROOT}/", "")}: missing anchor ##{expected_id} in #{resolved.sub("#{REPO_ROOT}/", "")}"
  end
end

if missing.empty?
  puts "docs links ok"
else
  warn missing.join("\n")
  exit 1
end
