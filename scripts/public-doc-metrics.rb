#!/usr/bin/env ruby
# frozen_string_literal: true

require "cgi"
require "json"
require "optparse"

module PublicDocMetrics
  MARKDOWN_SOURCES = ["README.md", "README.zh-CN.md"].freeze
  LOCALES = %w[en zh-CN].freeze
  ENTRY_HTML = [
    "site/index.html",
    "site/docs/index.html",
    "site/zh-CN/index.html",
    "site/zh-CN/docs/index.html",
    "site/404.html"
  ].freeze
  CONFIG_EXAMPLES = [
    "docs/examples/config/README.md",
    "docs/examples/config/anthropic.toml",
    "docs/examples/config/code-intelligence-rust.toml",
    "docs/examples/config/deepseek-basic.toml",
    "docs/examples/config/gemini.toml",
    "docs/examples/config/mcp-safe-defaults.toml",
    "docs/examples/config/openai-compatible.toml",
    "docs/examples/config/openai-responses.toml"
  ].freeze

  module_function

  def markdown_sources(root)
    locale_sources = LOCALES.flat_map do |locale|
      Dir.glob(File.join(root, "docs", locale, "*.md")).sort.map do |path|
        relative_path(root, path)
      end
    end
    MARKDOWN_SOURCES + locale_sources
  end

  def visible_markdown(markdown)
    text = markdown.gsub(/<!--.*?-->/m, " ")
    text = text.each_line.reject { |line| line.lstrip.start_with?("<!--") }.join
    text = text.gsub(/^```.*?^```\s*$/m, " ")
    text = text.gsub(/^~~~.*?^~~~\s*$/m, " ")
    text = text.gsub(/!\[([^\]]*)\]\([^)]*\)/, "\\1")
    text = text.gsub(/\[([^\]]+)\]\([^)]*\)/, "\\1")
    text = text.gsub(/<[^>]+>/, " ")
    text = text.gsub(/[`*_>#|~]/, " ")
    text.gsub(/\s+/, " ").strip
  end

  def visible_html(html)
    text = html.gsub(/<!--.*?-->/m, " ")
    text = text.gsub(/<(script|style|pre)\b[^>]*>.*?<\/\1>/mi, " ")
    text = text.gsub(/<[^>]+>/, " ")
    CGI.unescapeHTML(text).gsub(/\s+/, " ").strip
  end

  def word_count(text)
    text.scan(/[\p{L}\p{N}]+(?:[-'][\p{L}\p{N}]+)*/u).length
  end

  def count_matches(content, pattern)
    content.scan(pattern).length
  end

  def count_nested_items(content, container_class, tag)
    opening = content.match(/<div\b[^>]*class=(["'])[^"']*\b#{Regexp.escape(container_class)}\b[^"']*\1[^>]*>/mi)
    return 0 unless opening

    depth = 1
    cursor = opening.end(0)
    inner = nil
    tag_pattern = /<\/?div\b[^>]*>/i
    while (match = tag_pattern.match(content, cursor))
      if match[0].start_with?("</")
        depth -= 1
        if depth.zero?
          inner = content[opening.end(0)...match.begin(0)]
          break
        end
      else
        depth += 1
      end
      cursor = match.end(0)
    end
    return 0 unless inner

    count_matches(inner, /<#{Regexp.escape(tag)}\b/i)
  end

  def collect(root)
    markdown = markdown_sources(root).each_with_object({}) do |path, result|
      source = File.read(File.join(root, path), encoding: "UTF-8")
      visible = visible_markdown(source)
      result[path] = {
        "visible_words" => word_count(visible),
        "visible_characters" => visible.length
      }
    end

    html = ENTRY_HTML.each_with_object({}) do |path, result|
      source = File.read(File.join(root, path), encoding: "UTF-8")
      visible = visible_html(source)
      result[path] = {
        "visible_words" => word_count(visible),
        "visible_characters" => visible.length,
        "feature_cards" => count_nested_items(source, "feature-grid", "article"),
        "resource_cards" => count_matches(source, /<a\b[^>]*class="[^"]*\bresource-card\b[^"]*"/i)
      }
    end

    {
      "schema_version" => 1,
      "normalization" => {
        "markdown" => "visible prose; fenced code, HTML comments, link targets, and markup excluded",
        "html" => "visible text; comments, script, style, pre, and markup excluded",
        "words" => "Unicode letter/number tokens with internal hyphen or apostrophe"
      },
      "scope" => {
        "markdown_sources" => markdown.length,
        "config_example_assets" => CONFIG_EXAMPLES.length,
        "hand_authored_html" => ENTRY_HTML.length
      },
      "totals" => {
        "docs_en_visible_words" => total_for_prefix(markdown, "docs/en/", "visible_words"),
        "docs_zh_cn_visible_words" => total_for_prefix(markdown, "docs/zh-CN/", "visible_words"),
        "root_readme_en_visible_words" => markdown.fetch("README.md").fetch("visible_words"),
        "root_readme_zh_cn_visible_words" => markdown.fetch("README.zh-CN.md").fetch("visible_words")
      },
      "markdown" => markdown,
      "html" => html,
      "config_examples" => CONFIG_EXAMPLES
    }
  end

  def total_for_prefix(entries, prefix, field)
    entries.sum { |path, values| path.start_with?(prefix) ? values.fetch(field) : 0 }
  end

  def relative_path(root, path)
    path.sub(%r{\A#{Regexp.escape(File.expand_path(root))}/?}, "")
  end
end

if $PROGRAM_NAME == __FILE__
  options = { root: File.expand_path("..", __dir__), output: nil }
  OptionParser.new do |parser|
    parser.on("--root PATH") { |path| options[:root] = File.expand_path(path) }
    parser.on("--output PATH") { |path| options[:output] = File.expand_path(path) }
  end.parse!

  payload = JSON.pretty_generate(PublicDocMetrics.collect(options.fetch(:root))) + "\n"
  if options[:output]
    File.write(options.fetch(:output), payload)
  else
    puts payload
  end
end
