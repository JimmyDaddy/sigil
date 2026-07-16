#!/usr/bin/env ruby
# frozen_string_literal: true

require "cgi"
require "date"
require "fileutils"
require "json"
require "open3"

REPO_ROOT = File.expand_path("..", __dir__)
OUT_DIR = File.expand_path(ARGV.fetch(0), REPO_ROOT) if $PROGRAM_NAME == __FILE__
SITE_URL = "https://jimmydaddy.github.io/sigil"
SOURCE_LAST_MODIFIED_CACHE = {}

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
  ["permissions-and-sandbox", "permissions-and-sandbox.md", "Permissions and sandbox"],
  ["appearance", "appearance.md", "Appearance"],
  ["advanced-configuration", "advanced-configuration.md", "Advanced configuration"],
  ["configuration-reference", "configuration-reference.md", "Configuration reference"],
  ["providers", "providers.md", "Provider guide"],
  ["provider-deepseek", "provider-deepseek.md", "DeepSeek provider"],
  ["provider-openai-compatible", "provider-openai-compatible.md", "OpenAI-compatible provider"],
  ["provider-openai-responses", "provider-openai-responses.md", "OpenAI Responses provider"],
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

NAV_GROUPS = [
  ["get-started", %w[overview quickstart installation visual-tour]],
  ["use-sigil", %w[workflows cookbook user-guide reference]],
  ["configure-sigil", %w[configuration permissions-and-sandbox appearance advanced-configuration configuration-reference]],
  ["providers-and-integrations", %w[providers provider-deepseek provider-openai-compatible provider-openai-responses provider-anthropic provider-gemini mcp terminal-compatibility]],
  ["safety-and-troubleshooting", %w[safety privacy troubleshooting]],
  ["project-status", %w[status changelog]]
].freeze

NAV_GROUP_TITLES = {
  "en" => {
    "get-started" => "Get started",
    "use-sigil" => "Use Sigil",
    "configure-sigil" => "Configure Sigil",
    "providers-and-integrations" => "Providers and integrations",
    "safety-and-troubleshooting" => "Safety and troubleshooting",
    "project-status" => "Project status"
  },
  "zh-CN" => {
    "get-started" => "开始使用",
    "use-sigil" => "使用 Sigil",
    "configure-sigil" => "配置 Sigil",
    "providers-and-integrations" => "Provider 与集成",
    "safety-and-troubleshooting" => "安全与排障",
    "project-status" => "项目状态"
  }
}.freeze

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
  "permissions-and-sandbox" => "权限与沙箱",
  "appearance" => "外观",
  "advanced-configuration" => "高级配置",
  "configuration-reference" => "配置字段参考",
  "providers" => "Provider 指南",
  "provider-deepseek" => "DeepSeek provider",
  "provider-openai-compatible" => "OpenAI-compatible provider",
  "provider-openai-responses" => "OpenAI Responses provider",
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

README_PAGE_TITLE = {
  "en" => "Docs home",
  "zh-CN" => "文档首页"
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
    skip_label: "Skip to content",
    menu_label: "Menu",
    primary_nav_label: "Primary navigation",
    docs_nav_label: "Documentation navigation",
    docs_menu_label: "Docs menu",
    toc_label: "On this page",
    previous_next_label: "Previous and next pages",
    home_aria_label: "Sigil home",
    search_label: "Search docs",
    search_results_label: "Search results",
    search_placeholder: "Search providers, config, approvals...",
    version_notice_label: "Development documentation",
    version_notice_text: "These pages track main. The packaged alpha is v0.0.1-alpha.4; newer features may require a source install.",
    version_notice_link: "Review Unreleased"
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
    skip_label: "跳到正文",
    menu_label: "菜单",
    primary_nav_label: "主导航",
    docs_nav_label: "文档导航",
    docs_menu_label: "文档目录",
    toc_label: "本页内容",
    previous_next_label: "上一篇和下一篇",
    home_aria_label: "Sigil 首页",
    search_label: "搜索文档",
    search_results_label: "搜索结果",
    search_placeholder: "搜索 provider、配置、审批...",
    version_notice_label: "开发版本文档",
    version_notice_text: "这些页面跟随 main。已打包发布的 alpha 是 v0.0.1-alpha.4；较新的能力可能需要从源码安装。",
    version_notice_link: "查看 Unreleased"
  }
}.freeze

def page_slug_for_file(file)
  basename = File.basename(file)
  page = PAGES.find { |(_, filename, _)| filename == basename }
  page&.first
end

def page_title_for_file(file, locale)
  basename = File.basename(file)
  return README_PAGE_TITLE.fetch(locale, "Docs home") if basename == "README.md"

  slug, _filename, en_title = PAGES.find { |_, filename, _| filename == basename }
  return nil unless slug

  locale == "zh-CN" ? ZH_PAGE_TITLES.fetch(slug, en_title) : en_title
end

def normalize_link_label(label, href, locale)
  return label if href.start_with?("http://", "https://", "mailto:", "#")

  target, = href.split("#", 2)
  target_file = File.basename(target)
  return label unless target_file.end_with?(".md")

  base_label = File.basename(label)
  return label unless base_label == target_file || label.end_with?(".md")

  page_title_for_file(target_file, locale) || target_file.sub(/\.md\z/, "")
end

def relative_prefix(locale)
  locale == "en" ? "../.." : "../../.."
end

def search_form_html(locale, asset_prefix, id_suffix)
  locale_config = LOCALES.fetch(locale)
  input_id = "doc-search-#{locale.gsub(/[^a-zA-Z0-9]/, "-")}-#{id_suffix.gsub(/[^a-zA-Z0-9]/, "-")}"
  results_id = "#{input_id}-results"
  <<~HTML
    <form class="site-search" role="search" data-locale="#{html_escape(locale)}" data-index="#{asset_prefix}/search.json">
      <label for="#{input_id}">#{html_escape(locale_config.fetch(:search_label))}</label>
      <input id="#{input_id}" type="search" name="q" autocomplete="off" aria-controls="#{results_id}" aria-autocomplete="list" placeholder="#{html_escape(locale_config.fetch(:search_placeholder))}">
      <div class="search-results" id="#{results_id}" aria-label="#{html_escape(locale_config.fetch(:search_results_label))}" aria-live="polite"></div>
    </form>
  HTML
end

def theme_boot_script
  <<~HTML
    <script>
      (() => {
        try {
          const theme = localStorage.getItem("sigil.theme");
          if (theme === "dark" || theme === "light") {
            document.documentElement.dataset.theme = theme;
          }
        } catch (_error) {}
      })();
    </script>
  HTML
end

def theme_toggle_html(locale)
  label = locale == "zh-CN" ? "切换到深色主题" : "Switch to dark theme"
  %(<button class="theme-toggle" type="button" data-theme-toggle aria-label="#{html_escape(label)}" aria-pressed="false" title="#{html_escape(label)}">☾</button>)
end

def brand_html(asset_prefix, home_href, locale = "en")
  label = LOCALES.fetch(locale).fetch(:home_aria_label)
  <<~HTML
    <a class="brand" href="#{home_href}" aria-label="#{html_escape(label)}">
      <img class="brand-mark" src="#{asset_prefix}/assets/logo/sigil-mark-staff-glow.svg" alt="" width="34" height="40">
      <span class="brand-wordmark" aria-hidden="true"></span>
    </a>
  HTML
end

def page_url(locale, slug)
  locale_config = LOCALES.fetch(locale)
  "#{locale_config.fetch(:site_prefix)}/#{slug}/"
end

def source_last_modified(source_path)
  absolute_path = File.expand_path(source_path)
  return SOURCE_LAST_MODIFIED_CACHE.fetch(absolute_path) if SOURCE_LAST_MODIFIED_CACHE.key?(absolute_path)

  relative_path = absolute_path.delete_prefix("#{REPO_ROOT}/")
  git_date = nil
  if File.exist?(File.join(REPO_ROOT, ".git"))
    output, status = Open3.capture2e(
      "git",
      "log",
      "-1",
      "--format=%cI",
      "--",
      relative_path,
      chdir: REPO_ROOT
    )
    git_date = Date.parse(output.strip).iso8601 if status.success? && !output.strip.empty?
  end

  SOURCE_LAST_MODIFIED_CACHE[absolute_path] = git_date || File.mtime(absolute_path).utc.to_date.iso8601
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

def unique_heading_id(text, counts)
  base = slugify(text)
  counts[base] += 1
  counts[base] == 1 ? base : "#{base}-#{counts[base]}"
end

def inline_markdown(text, locale)
  escaped = html_escape(text)
  escaped = escaped.gsub(/`([^`]+)`/) { "<code>#{Regexp.last_match(1)}</code>" }
  escaped = escaped.gsub(/\*\*([^*]+)\*\*/) { "<strong>#{Regexp.last_match(1)}</strong>" }
  escaped = escaped.gsub(/!\[([^\]]*)\]\(([^)]+)\)/) do
    alt = Regexp.last_match(1)
    href = Regexp.last_match(2)
    %(<img src="#{rewrite_href(href, locale)}" alt="#{alt}" loading="lazy" decoding="async">)
  end
  escaped.gsub(/\[([^\]]+)\]\(([^)]+)\)/) do
    label = Regexp.last_match(1)
    href = Regexp.last_match(2)
    pretty_label = normalize_link_label(label, href, locale)
    safe_label = pretty_label == label ? label : html_escape(pretty_label)
    %(<a href="#{rewrite_href(href, locale)}">#{safe_label}</a>)
  end
end

def table_cells(line)
  content = line.strip
  content = content[1..] if content.start_with?("|")

  cells = []
  cell = +""
  code_delimiter = nil
  index = 0

  while index < content.length
    character = content[index]

    if character == "\\" && content[index + 1] == "|"
      cell << "|"
      index += 2
      next
    end

    if character == "`"
      run_end = index
      run_end += 1 while content[run_end] == "`"
      delimiter = content[index...run_end]
      if code_delimiter.nil?
        code_delimiter = delimiter
      elsif code_delimiter == delimiter
        code_delimiter = nil
      end
      cell << delimiter
      index = run_end
      next
    end

    if character == "|" && code_delimiter.nil?
      cells << cell.strip
      cell = +""
    else
      cell << character
    end
    index += 1
  end

  cells << cell.strip
  cells.pop if cell.empty? && content.end_with?("|") && code_delimiter.nil?
  cells
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
  state = {
    code: false,
    code_lang: "",
    code_lines: [],
    ul: false,
    ol: false,
    table: false,
    table_header: false,
    heading_counts: Hash.new(0)
  }
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
        code = html_escape(state[:code_lines].join("\n"))
        html << %(<pre><code class="language-#{html_escape(state[:code_lang])}">#{code}</code></pre>)
        state[:code] = false
        state[:code_lang] = ""
        state[:code_lines].clear
      else
        state[:code_lines] << line
      end
      next
    end

    if line.start_with?("```")
      flush_paragraph.call
      close_lists(html, state)
      close_table(html, state)
      lang = line.sub("```", "").strip
      state[:code] = true
      state[:code_lang] = lang
      state[:code_lines].clear
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
      id = unique_heading_id(text, state[:heading_counts])
      toc << [level, id, text] if level <= 3
      html << %(<h#{level} id="#{id}">#{inline_markdown(text, locale)}</h#{level}>)
      next
    end

    if line.include?("|") && line.strip.start_with?("|")
      flush_paragraph.call
      close_lists(html, state)
      cells = table_cells(line)
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
  if state[:code]
    code = html_escape(state[:code_lines].join("\n"))
    html << %(<pre><code class="language-#{html_escape(state[:code_lang])}">#{code}</code></pre>)
  end
  [html.join("\n"), toc]
end

def nav_html(locale, active_slug)
  pages_by_slug = PAGES.to_h { |slug, file, title| [slug, [file, title]] }
  NAV_GROUPS.map do |group_key, slugs|
    group_id = "doc-nav-group-#{group_key}"
    links = slugs.map do |slug|
      _file, title = pages_by_slug.fetch(slug)
      href = slug == active_slug ? "./" : "../#{slug}/"
      klass = slug == active_slug ? " class=\"active\"" : ""
      current = slug == active_slug ? ' aria-current="page"' : ""
      label = locale == "zh-CN" ? ZH_PAGE_TITLES.fetch(slug, title) : title
      %(<a#{klass}#{current} href="#{href}">#{html_escape(label)}</a>)
    end.join("\n")
    title = NAV_GROUP_TITLES.fetch(locale).fetch(group_key)
    <<~HTML
      <section class="doc-nav-group">
        <h2 class="doc-nav-group-title" id="#{group_id}">#{html_escape(title)}</h2>
        <nav class="doc-nav-group-links" aria-labelledby="#{group_id}">
          #{links}
        </nav>
      </section>
    HTML
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

def truncate_text(text, limit = 180)
  normalized = text.to_s.gsub(/\s+/, " ").strip
  return normalized if normalized.length <= limit

  truncated = normalized[0, limit].rstrip
  if truncated.include?(" ")
    word_boundary = truncated.rindex(" ")
    truncated = truncated[0, word_boundary] if word_boundary && word_boundary >= limit / 2
  end
  "#{truncated}…"
end

def page_description(markdown, fallback_title)
  markdown.lines.each do |line|
    stripped = line.strip
    next if stripped.empty?
    next if stripped.start_with?("#", "[", "```", "|")
    next if stripped.match?(/\A[-*]\s+/)

    return truncate_text(plain_text_from_markdown(stripped))
  end
  fallback_title
end

def section_search_items(markdown, locale, page_title, base_url)
  counts = Hash.new(0)
  sections = []
  current = nil
  in_code = false

  markdown.each_line do |raw_line|
    line = raw_line.chomp
    if line.start_with?("```")
      in_code = !in_code
      next
    end
    next if in_code

    if (match = line.match(/^(#+)\s+(.+)$/))
      sections << current if current
      level = [match[1].length, 6].min
      title = plain_text_from_markdown(match[2])
      id = unique_heading_id(match[2], counts)
      current = if (2..3).cover?(level)
                  { title: title, id: id, lines: [] }
                end
      next
    end

    current[:lines] << line if current
  end
  sections << current if current

  sections.compact.map do |section|
    text = plain_text_from_markdown(section.fetch(:lines).join("\n"))
    {
      "kind" => "section",
      "locale" => locale,
      "title" => page_title,
      "section" => section.fetch(:title),
      "description" => truncate_text(text.empty? ? section.fetch(:title) : text),
      "url" => "#{base_url}##{section.fetch(:id)}",
      "text" => "#{section.fetch(:title)} #{text}".strip
    }
  end
end

def rendered_page(locale, slug, source_file, fallback_title)
  locale_config = LOCALES.fetch(locale)
  source_path = File.join(REPO_ROOT, locale_config.fetch(:source_dir), source_file)
  markdown = File.read(source_path)
  last_modified = source_last_modified(source_path)
  body, toc = render_markdown(markdown, locale)
  title = markdown[/^#\s+(.+)$/, 1] || fallback_title
  description = page_description(markdown, title)
  asset_prefix = relative_prefix(locale)
  language_href = locale == "en" ? "../../zh-CN/docs/#{slug}/" : "../../../docs/#{slug}/"
  home_href = "../../"
  docs_home = locale == "en" ? "../" : "../"
  canonical = "#{SITE_URL}/#{page_url(locale, slug)}"
  alternate_en = "#{SITE_URL}/#{page_url("en", slug)}"
  alternate_zh = "#{SITE_URL}/#{page_url("zh-CN", slug)}"
  toc_html = toc.select { |level, _id, _text| (2..3).cover?(level) }.map do |level, id, text|
    %(<a class="level-#{level}" href="##{id}">#{html_escape(text.gsub(/[`*_]/, ""))}</a>)
  end.join("\n")
  json_ld = {
    "@context" => "https://schema.org",
    "@type" => "TechArticle",
    "headline" => title,
    "description" => description,
    "url" => canonical,
    "dateModified" => last_modified,
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
        <link rel="icon" href="#{asset_prefix}/assets/logo/sigil-mark-staff-glow.svg">
        <meta name="theme-color" content="#1ecfc5">
        <meta property="og:type" content="article">
        <meta property="og:site_name" content="Sigil">
        <meta property="og:title" content="#{html_escape(title)} - Sigil">
        <meta property="og:description" content="#{html_escape(description)}">
        <meta property="og:url" content="#{canonical}">
        <meta property="og:image" content="#{SITE_URL}/assets/logo/sigil-full-staff-glow.png">
        <meta name="twitter:card" content="summary_large_image">
        <meta name="twitter:title" content="#{html_escape(title)} - Sigil">
        <meta name="twitter:description" content="#{html_escape(description)}">
        <meta name="twitter:image" content="#{SITE_URL}/assets/logo/sigil-full-staff-glow.png">
        <script type="application/ld+json">#{JSON.generate(json_ld)}</script>
        #{theme_boot_script}
        <link rel="stylesheet" href="#{asset_prefix}/assets/site.css">
        <script defer src="#{asset_prefix}/assets/site.js"></script>
        <script defer src="#{asset_prefix}/assets/code.js"></script>
        <script defer src="#{asset_prefix}/assets/search.js"></script>
      </head>
      <body class="doc-page">
        <a class="skip-link" href="#main-content">#{html_escape(locale_config.fetch(:skip_label))}</a>
        <header class="site-header">
          #{brand_html(asset_prefix, home_href, locale)}
          <div class="header-actions">
            <details class="nav-menu">
              <summary>#{html_escape(locale_config.fetch(:menu_label))}</summary>
              <nav aria-label="#{html_escape(locale_config.fetch(:primary_nav_label))}">
                <a href="#{home_href}#workflow">#{html_escape(locale_config.fetch(:workflow_label))}</a>
                <a href="#{home_href}#safety">#{html_escape(locale_config.fetch(:safety_label))}</a>
                <a href="#{docs_home}" aria-current="page">#{html_escape(locale_config.fetch(:docs_label))}</a>
                <a href="#{language_href}">#{html_escape(locale_config.fetch(:language_label))}</a>
                <a class="nav-cta" href="https://github.com/JimmyDaddy/sigil">#{html_escape(locale_config.fetch(:github_label))}</a>
              </nav>
            </details>
            #{theme_toggle_html(locale)}
          </div>
        </header>
        <main class="doc-shell" id="main-content">
          <aside class="doc-sidebar" aria-label="#{html_escape(locale_config.fetch(:docs_nav_label))}">
            <details class="doc-navigation">
              <summary>#{html_escape(locale_config.fetch(:docs_menu_label))}</summary>
              <div class="doc-navigation-panel">
                #{search_form_html(locale, asset_prefix, slug)}
                <a class="doc-home-link" href="#{docs_home}">#{html_escape(locale_config.fetch(:docs_label))}</a>
                #{nav_html(locale, slug)}
              </div>
            </details>
          </aside>
          <article class="doc-content">
            <div class="doc-meta">
              <a href="#{language_href}">#{html_escape(locale_config.fetch(:language_label))}</a>
            </div>
            <aside class="docs-version-notice" aria-label="#{html_escape(locale_config.fetch(:version_notice_label))}">
              <strong>#{html_escape(locale_config.fetch(:version_notice_label))}</strong>
              <span>#{html_escape(locale_config.fetch(:version_notice_text))}</span>
              <a href="../changelog/">#{html_escape(locale_config.fetch(:version_notice_link))}</a>
            </aside>
            <details class="doc-toc-mobile">
              <summary>#{html_escape(locale_config.fetch(:toc_label))}</summary>
              <nav aria-label="#{html_escape(locale_config.fetch(:toc_label))}">
                #{toc_html}
              </nav>
            </details>
            #{body}
            <nav class="doc-page-nav" aria-label="#{html_escape(locale_config.fetch(:previous_next_label))}">
              #{sibling_nav(locale, slug)}
            </nav>
          </article>
          <aside class="doc-toc" aria-label="#{html_escape(locale_config.fetch(:toc_label))}">
            <strong class="doc-toc-title">#{html_escape(locale_config.fetch(:toc_label))}</strong>
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
        "kind" => "page",
        "locale" => locale,
        "title" => page_title,
        "description" => page_description(markdown, page_title),
        "url" => page_url(locale, slug),
        "text" => plain_text_from_markdown(markdown)
      }
      items.concat(section_search_items(markdown, locale, page_title, page_url(locale, slug)))
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
    ["", 1.0, File.join(REPO_ROOT, "site", "index.html")],
    ["zh-CN/", 0.9, File.join(REPO_ROOT, "site", "zh-CN", "index.html")],
    ["docs/", 0.9, File.join(REPO_ROOT, "site", "docs", "index.html")],
    ["zh-CN/docs/", 0.9, File.join(REPO_ROOT, "site", "zh-CN", "docs", "index.html")]
  ]
  PAGES.each do |slug, file, _title|
    urls << [page_url("en", slug), 0.75, File.join(REPO_ROOT, "docs", "en", file)]
    urls << [page_url("zh-CN", slug), 0.75, File.join(REPO_ROOT, "docs", "zh-CN", file)]
  end
  body = urls.map do |path, priority, source_path|
    loc = path.empty? ? "#{SITE_URL}/" : "#{SITE_URL}/#{path}"
    <<~XML
      <url>
        <loc>#{loc}</loc>
        <lastmod>#{source_last_modified(source_path)}</lastmod>
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
        <meta name="theme-color" content="#1ecfc5">
        #{theme_boot_script}
        <link rel="stylesheet" href="../../assets/site.css">
        <script defer src="../../assets/site.js"></script>
        <script defer src="../../assets/code.js"></script>
      </head>
      <body class="doc-page">
        <a class="skip-link" href="#main-content">Skip to content</a>
        <header class="site-header">
          #{brand_html("../..", "../../", "en")}
          <div class="header-actions">
            <details class="nav-menu">
              <summary>Menu</summary>
              <nav aria-label="Primary navigation">
                <a href="../../docs/" aria-current="page">Docs</a>
                <a href="https://github.com/JimmyDaddy/sigil">GitHub</a>
              </nav>
            </details>
            #{theme_toggle_html("en")}
          </div>
        </header>
        <main class="doc-shell examples-shell" id="main-content">
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

if $PROGRAM_NAME == __FILE__
  write_pages
  write_examples_index
  write_search_index
  write_sitemap
  puts "generated docs pages in #{OUT_DIR}"
end
