#!/usr/bin/env ruby
# frozen_string_literal: true

require "cgi"
require "date"
require "fileutils"
require "json"

REPO_ROOT = File.expand_path("..", __dir__)
OUT_DIR = File.expand_path(ARGV.fetch(0), REPO_ROOT)
SITE_URL = "https://jimmydaddy.github.io/sigil"
LASTMOD = Date.today.iso8601

PAGES = [
  ["overview", "README.md", "User docs"],
  ["quickstart", "quickstart.md", "Quickstart"],
  ["installation", "installation.md", "Installation"],
  ["visual-tour", "visual-tour.md", "Visual tour"],
  ["workflows", "workflows.md", "Common workflows"],
  ["cookbook", "cookbook.md", "Cookbook"],
  ["user-guide", "user-guide.md", "TUI user guide"],
  ["safety", "safety.md", "Safety and permissions"],
  ["configuration", "configuration.md", "Configuration"],
  ["providers", "providers.md", "Provider guide"],
  ["provider-deepseek", "provider-deepseek.md", "DeepSeek provider"],
  ["provider-openai-compatible", "provider-openai-compatible.md", "OpenAI-compatible provider"],
  ["provider-anthropic", "provider-anthropic.md", "Anthropic provider"],
  ["provider-gemini", "provider-gemini.md", "Gemini provider"],
  ["privacy", "privacy.md", "Privacy and data handling"],
  ["troubleshooting", "troubleshooting.md", "Troubleshooting"],
  ["reference", "reference.md", "Command and key reference"],
  ["mcp", "mcp.md", "MCP guide"],
  ["terminal-compatibility", "terminal-compatibility.md", "Terminal compatibility"],
  ["status", "status.md", "Supported today and future work"],
  ["changelog", "changelog.md", "User changelog"]
].freeze

ZH_PAGE_TITLES = {
  "overview" => "用户文档",
  "quickstart" => "快速开始",
  "installation" => "安装",
  "visual-tour" => "视觉导览",
  "workflows" => "常见工作流",
  "cookbook" => "任务手册",
  "user-guide" => "TUI 用户指南",
  "safety" => "安全与权限",
  "configuration" => "配置",
  "providers" => "Provider 指南",
  "provider-deepseek" => "DeepSeek provider",
  "provider-openai-compatible" => "OpenAI-compatible provider",
  "provider-anthropic" => "Anthropic provider",
  "provider-gemini" => "Gemini provider",
  "privacy" => "隐私与数据处理",
  "troubleshooting" => "故障排查",
  "reference" => "命令与快捷键参考",
  "mcp" => "MCP 指南",
  "terminal-compatibility" => "终端兼容性",
  "status" => "当前支持与后续计划",
  "changelog" => "用户变更记录"
}.freeze

LOCALES = {
  "en" => {
    source_dir: "docs/en",
    site_prefix: "docs",
    html_lang: "en",
    home_label: "Home",
    docs_label: "Docs",
    workflow_label: "Workflow",
    safety_label: "Safety",
    github_label: "GitHub",
    language_label: "简体中文",
    previous_label: "Previous",
    next_label: "Next",
    search_label: "Search docs",
    search_placeholder: "Search providers, config, approvals..."
  },
  "zh-CN" => {
    source_dir: "docs/zh-CN",
    site_prefix: "zh-CN/docs",
    html_lang: "zh-CN",
    home_label: "首页",
    docs_label: "文档",
    workflow_label: "工作流",
    safety_label: "安全",
    github_label: "GitHub",
    language_label: "English",
    previous_label: "上一篇",
    next_label: "下一篇",
    search_label: "搜索文档",
    search_placeholder: "搜索 provider、配置、审批..."
  }
}.freeze

def page_slug_for_file(file)
  basename = File.basename(file)
  page = PAGES.find { |(_, filename, _)| filename == basename }
  page&.first
end

def relative_prefix(locale)
  locale == "en" ? "../.." : "../../.."
end

def search_form_html(locale, asset_prefix, id_suffix)
  locale_config = LOCALES.fetch(locale)
  input_id = "doc-search-#{locale.gsub(/[^a-zA-Z0-9]/, "-")}-#{id_suffix.gsub(/[^a-zA-Z0-9]/, "-")}"
  <<~HTML
    <form class="site-search" role="search" data-locale="#{html_escape(locale)}" data-index="#{asset_prefix}/search.json">
      <label for="#{input_id}">#{html_escape(locale_config.fetch(:search_label))}</label>
      <input id="#{input_id}" type="search" name="q" autocomplete="off" placeholder="#{html_escape(locale_config.fetch(:search_placeholder))}">
      <div class="search-results" aria-live="polite"></div>
    </form>
  HTML
end

def page_url(locale, slug)
  locale_config = LOCALES.fetch(locale)
  "#{locale_config.fetch(:site_prefix)}/#{slug}/"
end

def html_escape(value)
  CGI.escapeHTML(value.to_s)
end

def slugify(text)
  slug = text.downcase.strip.gsub(/<[^>]+>/, "")
  slug = slug.gsub(/[`*_~]/, "")
  slug = slug.gsub(/[^\p{Alnum}\p{Han}\s-]/, "")
  slug = slug.gsub(/\s+/, "-").gsub(/-+/, "-").gsub(/\A-|-+\z/, "")
  slug.empty? ? "section" : slug
end

def inline_markdown(text, locale)
  escaped = html_escape(text)
  escaped = escaped.gsub(/`([^`]+)`/) { "<code>#{Regexp.last_match(1)}</code>" }
  escaped = escaped.gsub(/\*\*([^*]+)\*\*/) { "<strong>#{Regexp.last_match(1)}</strong>" }
  escaped = escaped.gsub(/!\[([^\]]*)\]\(([^)]+)\)/) do
    alt = Regexp.last_match(1)
    href = Regexp.last_match(2)
    %(<img src="#{html_escape(rewrite_href(href, locale))}" alt="#{alt}">)
  end
  escaped.gsub(/\[([^\]]+)\]\(([^)]+)\)/) do
    label = Regexp.last_match(1)
    href = Regexp.last_match(2)
    %(<a href="#{html_escape(rewrite_href(href, locale))}">#{label}</a>)
  end
end

def rewrite_href(href, locale)
  return href if href.start_with?("http://", "https://", "mailto:", "#")

  target, anchor = href.split("#", 2)
  rewritten =
    if target.start_with?("../../site/assets/")
      asset_path = target.sub("../../site/", "")
      locale == "en" ? "../../#{asset_path}" : "../../../#{asset_path}"
    elsif target.end_with?(".md")
      slug = page_slug_for_file(target)
      cross_zh = target.include?("../zh-CN/")
      cross_en = target.include?("../en/")
      if slug.nil?
        href
      elsif File.basename(target) == "README.md"
        if cross_zh
          "../../zh-CN/docs/"
        elsif cross_en
          "../../../docs/"
        else
          "../"
        end
      elsif cross_zh
        "../../zh-CN/docs/#{slug}/"
      elsif cross_en
        "../../../docs/#{slug}/"
      else
        "../#{slug}/"
      end
    elsif target.start_with?("../examples/")
      "#{relative_prefix(locale)}/#{target.sub("../", "")}"
    else
      href
    end

  anchor ? "#{rewritten}##{anchor}" : rewritten
end

def close_lists(html, state)
  if state[:ul]
    html << "</ul>"
    state[:ul] = false
  end
  return unless state[:ol]

  html << "</ol>"
  state[:ol] = false
end

def close_table(html, state)
  return unless state[:table]

  html << "</tbody></table>"
  state[:table] = false
end

def render_markdown(markdown, locale)
  html = []
  toc = []
  state = { code: false, ul: false, ol: false, table: false, table_header: false }
  paragraph = []

  flush_paragraph = lambda do
    next if paragraph.empty?

    html << "<p>#{inline_markdown(paragraph.join(" "), locale)}</p>"
    paragraph.clear
  end

  markdown.each_line do |raw_line|
    line = raw_line.chomp

    if state[:code]
      if line.start_with?("```")
        html << "</code></pre>"
        state[:code] = false
      else
        html << html_escape(line)
      end
      next
    end

    if line.start_with?("```")
      flush_paragraph.call
      close_lists(html, state)
      close_table(html, state)
      lang = line.sub("```", "").strip
      html << %(<pre><code class="language-#{html_escape(lang)}">)
      state[:code] = true
      next
    end

    if line.strip.empty?
      flush_paragraph.call
      close_lists(html, state)
      close_table(html, state)
      next
    end

    if (match = line.match(/^(#+)\s+(.+)$/))
      flush_paragraph.call
      close_lists(html, state)
      close_table(html, state)
      level = [match[1].length, 6].min
      text = match[2]
      id = slugify(text)
      toc << [level, id, text] if level <= 3
      html << %(<h#{level} id="#{id}">#{inline_markdown(text, locale)}</h#{level}>)
      next
    end

    if line.include?("|") && line.strip.start_with?("|")
      flush_paragraph.call
      close_lists(html, state)
      cells = line.strip.split("|")[1..-2].map(&:strip)
      next if cells.all? { |cell| cell.match?(/\A:?-{3,}:?\z/) }

      unless state[:table]
        html << "<table><tbody>"
        state[:table] = true
        state[:table_header] = true
      end
      tag = state[:table_header] ? "th" : "td"
      html << "<tr>#{cells.map { |cell| "<#{tag}>#{inline_markdown(cell, locale)}</#{tag}>" }.join}</tr>"
      state[:table_header] = false
      next
    end

    if (match = line.match(/^\s*[-*]\s+(.+)$/))
      flush_paragraph.call
      close_table(html, state)
      unless state[:ul]
        close_lists(html, state)
        html << "<ul>"
        state[:ul] = true
      end
      html << "<li>#{inline_markdown(match[1], locale)}</li>"
      next
    end

    if (match = line.match(/^\s*\d+\.\s+(.+)$/))
      flush_paragraph.call
      close_table(html, state)
      unless state[:ol]
        close_lists(html, state)
        html << "<ol>"
        state[:ol] = true
      end
      html << "<li>#{inline_markdown(match[1], locale)}</li>"
      next
    end

    paragraph << line.strip
  end

  flush_paragraph.call
  close_lists(html, state)
  close_table(html, state)
  html << "</code></pre>" if state[:code]
  [html.join("\n"), toc]
end

def nav_html(locale, active_slug)
  PAGES.map do |slug, _file, title|
    href = slug == active_slug ? "./" : "../#{slug}/"
    klass = slug == active_slug ? " class=\"active\"" : ""
    label = locale == "zh-CN" ? ZH_PAGE_TITLES.fetch(slug, title) : title
    %(<a#{klass} href="#{href}">#{html_escape(label)}</a>)
  end.join("\n")
end

def sibling_nav(locale, active_slug)
  index = PAGES.index { |slug, _file, _title| slug == active_slug }
  locale_config = LOCALES.fetch(locale)
  previous_page = index.positive? ? PAGES[index - 1] : nil
  next_page = index && index < PAGES.length - 1 ? PAGES[index + 1] : nil
  links = []
  if previous_page
    label = locale == "zh-CN" ? ZH_PAGE_TITLES.fetch(previous_page[0], previous_page[2]) : previous_page[2]
    links << %(<a href="../#{previous_page[0]}/">#{html_escape(locale_config.fetch(:previous_label))}: #{html_escape(label)}</a>)
  end
  if next_page
    label = locale == "zh-CN" ? ZH_PAGE_TITLES.fetch(next_page[0], next_page[2]) : next_page[2]
    links << %(<a href="../#{next_page[0]}/">#{html_escape(locale_config.fetch(:next_label))}: #{html_escape(label)}</a>)
  end
  links.join("\n")
end

def plain_text_from_markdown(markdown)
  markdown
    .gsub(/```[a-zA-Z0-9_-]*\n/, " ")
    .gsub(/```/, " ")
    .gsub(/!\[[^\]]*\]\([^)]+\)/, " ")
    .gsub(/\[([^\]]+)\]\([^)]+\)/, "\\1")
    .gsub(/[`*_>#|~-]/, " ")
    .gsub(/\s+/, " ")
    .strip
end

def page_description(markdown, fallback_title)
  markdown.lines.each do |line|
    stripped = line.strip
    next if stripped.empty?
    next if stripped.start_with?("#", "[", "```", "|")
    next if stripped.match?(/\A[-*]\s+/)

    return plain_text_from_markdown(stripped)[0, 180]
  end
  fallback_title
end

def rendered_page(locale, slug, source_file, fallback_title)
  locale_config = LOCALES.fetch(locale)
  source_path = File.join(REPO_ROOT, locale_config.fetch(:source_dir), source_file)
  markdown = File.read(source_path)
  body, toc = render_markdown(markdown, locale)
  title = markdown[/^#\s+(.+)$/, 1] || fallback_title
  description = page_description(markdown, title)
  asset_prefix = relative_prefix(locale)
  language_href = locale == "en" ? "../../zh-CN/docs/#{slug}/" : "../../../docs/#{slug}/"
  home_href = locale == "en" ? "../../" : "../../../"
  docs_home = locale == "en" ? "../" : "../"
  canonical = "#{SITE_URL}/#{page_url(locale, slug)}"
  alternate_en = "#{SITE_URL}/#{page_url("en", slug)}"
  alternate_zh = "#{SITE_URL}/#{page_url("zh-CN", slug)}"
  toc_html = toc.select { |level, _id, _text| level <= 3 }.map do |level, id, text|
    %(<a class="level-#{level}" href="##{id}">#{html_escape(text.gsub(/[`*_]/, ""))}</a>)
  end.join("\n")
  json_ld = {
    "@context" => "https://schema.org",
    "@type" => "TechArticle",
    "headline" => title,
    "description" => description,
    "url" => canonical,
    "dateModified" => LASTMOD,
    "publisher" => {
      "@type" => "Organization",
      "name" => "Sigil"
    }
  }

  <<~HTML
    <!doctype html>
    <html lang="#{locale_config.fetch(:html_lang)}">
      <head>
        <meta charset="utf-8">
        <meta name="viewport" content="width=device-width, initial-scale=1">
        <title>#{html_escape(title)} - Sigil</title>
        <meta name="description" content="#{html_escape(description)}">
        <link rel="canonical" href="#{canonical}">
        <link rel="alternate" hreflang="en" href="#{alternate_en}">
        <link rel="alternate" hreflang="zh-CN" href="#{alternate_zh}">
        <link rel="alternate" hreflang="x-default" href="#{alternate_en}">
        <link rel="icon" href="#{asset_prefix}/assets/logo/sigil-mark-square-1024.png">
        <meta name="theme-color" content="#1ecfc5">
        <meta property="og:type" content="article">
        <meta property="og:site_name" content="Sigil">
        <meta property="og:title" content="#{html_escape(title)} - Sigil">
        <meta property="og:description" content="#{html_escape(description)}">
        <meta property="og:url" content="#{canonical}">
        <meta property="og:image" content="#{SITE_URL}/assets/logo/sigil-full.png">
        <meta name="twitter:card" content="summary_large_image">
        <meta name="twitter:title" content="#{html_escape(title)} - Sigil">
        <meta name="twitter:description" content="#{html_escape(description)}">
        <meta name="twitter:image" content="#{SITE_URL}/assets/logo/sigil-full.png">
        <script type="application/ld+json">#{JSON.generate(json_ld)}</script>
        <link rel="stylesheet" href="#{asset_prefix}/assets/site.css">
        <script defer src="#{asset_prefix}/assets/search.js"></script>
      </head>
      <body class="doc-page">
        <header class="site-header">
          <a class="brand" href="#{home_href}" aria-label="Sigil home">
            <img class="brand-mark" src="#{asset_prefix}/assets/logo/sigil-mark-transparent.png" alt="" width="34" height="40">
            <img class="brand-wordmark" src="#{asset_prefix}/assets/logo/sigil-wordmark-header.png" alt="" width="78" height="34">
          </a>
          <nav aria-label="Primary navigation">
            <a href="#{home_href}#workflow">#{html_escape(locale_config.fetch(:workflow_label))}</a>
            <a href="#{home_href}#safety">#{html_escape(locale_config.fetch(:safety_label))}</a>
            <a href="#{docs_home}">#{html_escape(locale_config.fetch(:docs_label))}</a>
            <a href="#{language_href}">#{html_escape(locale_config.fetch(:language_label))}</a>
            <a class="nav-cta" href="https://github.com/JimmyDaddy/sigil">#{html_escape(locale_config.fetch(:github_label))}</a>
          </nav>
        </header>
        <main class="doc-shell">
          <aside class="doc-sidebar" aria-label="Documentation navigation">
            #{search_form_html(locale, asset_prefix, slug)}
            <a class="doc-home-link" href="#{docs_home}">#{html_escape(locale_config.fetch(:docs_label))}</a>
            #{nav_html(locale, slug)}
          </aside>
          <article class="doc-content">
            <div class="doc-meta">
              <a href="#{language_href}">#{html_escape(locale_config.fetch(:language_label))}</a>
            </div>
            #{body}
            <nav class="doc-page-nav" aria-label="Previous and next pages">
              #{sibling_nav(locale, slug)}
            </nav>
          </article>
          <aside class="doc-toc" aria-label="On this page">
            #{toc_html}
          </aside>
        </main>
      </body>
    </html>
  HTML
end

def write_search_index
  items = []
  LOCALES.each do |locale, locale_config|
    PAGES.each do |slug, file, title|
      source_path = File.join(REPO_ROOT, locale_config.fetch(:source_dir), file)
      markdown = File.read(source_path)
      page_title = markdown[/^#\s+(.+)$/, 1] || (locale == "zh-CN" ? ZH_PAGE_TITLES.fetch(slug, title) : title)
      items << {
        "locale" => locale,
        "title" => page_title,
        "description" => page_description(markdown, page_title),
        "url" => page_url(locale, slug),
        "text" => plain_text_from_markdown(markdown)
      }
    end
  end
  File.write(File.join(OUT_DIR, "search.json"), JSON.pretty_generate(items))
end

def write_pages
  LOCALES.each_key do |locale|
    PAGES.each do |slug, file, title|
      out_path = File.join(OUT_DIR, page_url(locale, slug), "index.html")
      FileUtils.mkdir_p(File.dirname(out_path))
      File.write(out_path, rendered_page(locale, slug, file, title))
    end
  end
end

def write_sitemap
  urls = [
    ["", 1.0],
    ["zh-CN/", 0.9],
    ["docs/", 0.9],
    ["zh-CN/docs/", 0.9]
  ]
  PAGES.each do |slug, _file, _title|
    urls << [page_url("en", slug), 0.75]
    urls << [page_url("zh-CN", slug), 0.75]
  end
  body = urls.map do |path, priority|
    loc = path.empty? ? "#{SITE_URL}/" : "#{SITE_URL}/#{path}"
    <<~XML
      <url>
        <loc>#{loc}</loc>
        <lastmod>#{LASTMOD}</lastmod>
        <changefreq>weekly</changefreq>
        <priority>#{priority}</priority>
      </url>
    XML
  end.join
  File.write(File.join(OUT_DIR, "sitemap.xml"), <<~XML)
    <?xml version="1.0" encoding="UTF-8"?>
    <urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
    #{body}</urlset>
  XML
end

def write_examples_index
  out_path = File.join(OUT_DIR, "examples", "config", "index.html")
  FileUtils.mkdir_p(File.dirname(out_path))
  examples = Dir[File.join(OUT_DIR, "examples", "config", "*.toml")].map { |path| File.basename(path) }.sort
  links = examples.map do |file|
    %(<li><a href="#{html_escape(file)}">#{html_escape(file)}</a></li>)
  end.join("\n")
  File.write(out_path, <<~HTML)
    <!doctype html>
    <html lang="en">
      <head>
        <meta charset="utf-8">
        <meta name="viewport" content="width=device-width, initial-scale=1">
        <title>Sigil config examples</title>
        <meta name="description" content="Copyable Sigil configuration examples for providers, MCP, and code intelligence.">
        <link rel="stylesheet" href="../../assets/site.css">
      </head>
      <body class="doc-page">
        <header class="site-header">
          <a class="brand" href="../../" aria-label="Sigil home">
            <img class="brand-mark" src="../../assets/logo/sigil-mark-transparent.png" alt="" width="34" height="40">
            <img class="brand-wordmark" src="../../assets/logo/sigil-wordmark-header.png" alt="" width="78" height="34">
          </a>
          <nav aria-label="Primary navigation">
            <a href="../../docs/">Docs</a>
            <a href="https://github.com/JimmyDaddy/sigil">GitHub</a>
          </nav>
        </header>
        <main class="doc-shell examples-shell">
          <article class="doc-content">
            <h1>Sigil config examples</h1>
            <p>Copy these examples as starting points, then review model names, paths, API key sources, and trust settings before use.</p>
            <ul>
              #{links}
            </ul>
          </article>
        </main>
      </body>
    </html>
  HTML
end

write_pages
write_examples_index
write_search_index
write_sitemap
puts "generated docs pages in #{OUT_DIR}"
