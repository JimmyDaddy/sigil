#!/usr/bin/env ruby
# frozen_string_literal: true

require "cgi"
require "find"
require "set"

def attributes(tag)
  tag.scan(/\s([:\w-]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+)))?/m).to_h do |name, double_quoted, single_quoted, bare|
    [name.downcase, CGI.unescapeHTML(double_quoted || single_quoted || bare || "")]
  end
end

def structural_markup(html)
  html
    .gsub(/<!--.*?-->/m, "")
    .gsub(%r{(<script\b[^>]*>).*?</script\s*>}im, '\\1')
    .gsub(%r{(<style\b[^>]*>).*?</style\s*>}im, '\\1')
end

def tags(html, name)
  structural_markup(html).scan(/<#{Regexp.escape(name)}\b[^>]*>/im)
end

def visible_text(value)
  CGI.unescapeHTML(value.gsub(/<[^>]+>/, " ")).gsub(/\s+/, " ").strip
end

site_root = File.expand_path(ARGV.fetch(0) do
  warn "usage: scripts/check-site-accessibility.rb <built-site-directory>"
  exit 2
end)

unless Dir.exist?(site_root)
  warn "site artifact directory does not exist: #{site_root}"
  exit 2
end

html_paths = []
Find.find(site_root) { |path| html_paths << path if File.file?(path) && File.extname(path) == ".html" }
errors = []

html_paths.sort.each do |path|
  page = path.delete_prefix("#{site_root}/")
  html = File.read(path)
  markup = structural_markup(html)
  html_tag = tags(markup, "html").first
  errors << "#{page}: missing document language" if html_tag.nil? || attributes(html_tag)["lang"].to_s.strip.empty?

  title = html[%r{<title\b[^>]*>(.*?)</title>}im, 1]
  errors << "#{page}: missing non-empty title" if title.nil? || visible_text(title).empty?
  description = html[%r{<meta\b(?=[^>]*\bname=["']description["'])[^>]*\bcontent=["']([^"']+)["'][^>]*>}im, 1]
  errors << "#{page}: missing non-empty meta description" if description.to_s.strip.empty?

  main_tags = tags(markup, "main")
  errors << "#{page}: expected exactly one main landmark, found #{main_tags.length}" unless main_tags.length == 1
  errors << "#{page}: main landmark must have id=main-content" if main_tags.length == 1 && attributes(main_tags.first)["id"] != "main-content"

  h1_count = tags(markup, "h1").length
  errors << "#{page}: expected exactly one h1, found #{h1_count}" unless h1_count == 1
  heading_levels = markup.scan(/<h([1-6])\b[^>]*>/i).flatten.map(&:to_i)
  heading_levels.each_cons(2) do |previous, current|
    errors << "#{page}: heading level jumps from h#{previous} to h#{current}" if current > previous + 1
  end

  tags(markup, "nav").each do |tag|
    attrs = attributes(tag)
    next unless attrs["aria-label"].to_s.strip.empty? && attrs["aria-labelledby"].to_s.strip.empty?

    errors << "#{page}: navigation landmark is missing an accessible name"
  end

  tags(markup, "img").each do |tag|
    errors << "#{page}: image is missing alt text" unless attributes(tag).key?("alt")
  end

  labels = tags(markup, "label").filter_map { |tag| attributes(tag)["for"] }.to_set
  tags(markup, "input").each do |tag|
    attrs = attributes(tag)
    next if %w[hidden submit reset button image].include?(attrs.fetch("type", "text").downcase)
    next unless attrs["aria-label"].to_s.strip.empty? && attrs["aria-labelledby"].to_s.strip.empty?
    next if labels.include?(attrs["id"])

    errors << "#{page}: input #{attrs.fetch("id", "(without id)").inspect} is missing a label"
  end

  markup.scan(%r{(<button\b[^>]*>)(.*?)</button>}im).each do |opening_tag, content|
    attrs = attributes(opening_tag)
    next unless attrs["aria-label"].to_s.strip.empty? && attrs["aria-labelledby"].to_s.strip.empty? && visible_text(content).empty?

    errors << "#{page}: button is missing an accessible name"
  end

  markup.scan(%r{<details\b[^>]*>(.*?)</details>}im).each do |content|
    errors << "#{page}: details element must begin with summary" unless content.first.match?(/\A\s*<summary\b/i)
  end
end

if errors.empty?
  puts "site accessibility baseline ok (#{html_paths.length} HTML pages)"
else
  warn errors.join("\n")
  exit 1
end
