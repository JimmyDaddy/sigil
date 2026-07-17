#!/usr/bin/env ruby
# frozen_string_literal: true

require "cgi"
require "find"
require "json"
require "uri"

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

def all_tags(html)
  structural_markup(html).scan(/<[a-z][^>]*>/im)
end

def class_token?(tag_attributes, token)
  tag_attributes.fetch("class", "").split.include?(token)
end

def relative_path(path, root)
  path.delete_prefix("#{root}/")
end

site_root = File.expand_path(ARGV.fetch(0) do
  warn "usage: scripts/check-site-structure.rb <built-site-directory>"
  exit 2
end)

unless Dir.exist?(site_root)
  warn "built site directory is missing: #{site_root}"
  exit 2
end

html_paths = []
Find.find(site_root) do |path|
  html_paths << path if File.file?(path) && File.extname(path) == ".html"
end
html_paths.sort!

failures = []
if html_paths.empty?
  failures << "built site contains no HTML pages"
end

html_paths.each do |path|
  page = relative_path(path, site_root)
  html = File.read(path)

  ids = all_tags(html).filter_map { |tag| attributes(tag)["id"] }
  duplicate_ids = ids.tally.select { |_id, count| count > 1 }
  duplicate_ids.each do |id, count|
    failures << "#{page}: duplicate id #{id.inspect} appears #{count} times"
  end

  skip_links = tags(html, "a").select do |tag|
    attrs = attributes(tag)
    class_token?(attrs, "skip-link") && attrs["href"] == "#main-content"
  end
  failures << "#{page}: missing .skip-link with href=\"#main-content\"" if skip_links.empty?

  main_targets = tags(html, "main").count { |tag| attributes(tag)["id"] == "main-content" }
  failures << "#{page}: expected one <main id=\"main-content\">, found #{main_targets}" unless main_targets == 1

  next unless page == "zh-CN/index.html"

  brand_links = tags(html, "a").select { |tag| class_token?(attributes(tag), "brand") }
  if brand_links.length != 1
    failures << "#{page}: expected exactly one brand home link, found #{brand_links.length}"
  elsif attributes(brand_links.first)["href"] != "./"
    failures << "#{page}: brand home link must stay on the Chinese homepage with href=\"./\""
  end
end

[
  "index.html",
  "zh-CN/index.html"
].each do |page|
  path = File.join(site_root, page)
  next unless File.file?(path)

  html = File.read(path)
  lockups = tags(html, "div").count { |tag| class_token?(attributes(tag), "hero-brand-lockup") }
  failures << "#{page}: expected one hero brand lockup, found #{lockups}" unless lockups == 1

  hero_fields = tags(html, "div").count { |tag| class_token?(attributes(tag), "hero-field") }
  failures << "#{page}: expected one decorative hero field, found #{hero_fields}" unless hero_fields == 1

  terminal_stages = tags(html, "div").count { |tag| attributes(tag).key?("data-terminal-stage") }
  failures << "#{page}: expected one interactive terminal stage, found #{terminal_stages}" unless terminal_stages == 1

  terminal_signals = tags(html, "i").count { |tag| class_token?(attributes(tag), "signal-dot") }
  failures << "#{page}: expected three terminal status signals, found #{terminal_signals}" unless terminal_signals == 3

  capability_rails = tags(html, "div").count { |tag| class_token?(attributes(tag), "capability-rail") }
  failures << "#{page}: capability rail must stay removed, found #{capability_rails}" unless capability_rails.zero?

  capability_motion_toggles = tags(html, "button").count do |tag|
    attributes(tag).key?("data-capability-motion-toggle")
  end
  unless capability_motion_toggles.zero?
    failures << "#{page}: capability motion toggle must stay removed, found #{capability_motion_toggles}"
  end

  timeline_phases = tags(html, "li").filter_map do |tag|
    attrs = attributes(tag)
    attrs["data-phase"] if class_token?(attrs, "session-phase")
  end
  expected_phases = %w[planner worker tool verify status]
  unless timeline_phases == expected_phases
    failures << "#{page}: session timeline phases must be #{expected_phases.inspect}, found #{timeline_phases.inspect}"
  end

  decks = tags(html, "div").count { |tag| class_token?(attributes(tag), "terminal-deck") }
  failures << "#{page}: expected one layered terminal deck, found #{decks}" unless decks == 1
  %w[
    terminal-window-main
    terminal-window-approval
    terminal-window-verification
  ].each do |window_class|
    count = tags(html, "a").count { |tag| class_token?(attributes(tag), window_class) }
    failures << "#{page}: expected one #{window_class}, found #{count}" unless count == 1
  end
end

[
  "docs/index.html",
  "zh-CN/docs/index.html"
].each do |page|
  path = File.join(site_root, page)
  next unless File.file?(path)

  html = File.read(path)
  lockups = tags(html, "div").count { |tag| class_token?(attributes(tag), "docs-brand-lockup") }
  failures << "#{page}: expected one docs brand lockup, found #{lockups}" unless lockups == 1

  command_palettes = tags(html, "form").count { |tag| class_token?(attributes(tag), "docs-command-palette") }
  failures << "#{page}: expected one docs command palette, found #{command_palettes}" unless command_palettes == 1

  task_cards = tags(html, "a").filter_map do |tag|
    attrs = attributes(tag)
    [attrs["data-step"], attrs["href"]] if class_token?(attrs, "task-card")
  end
  expected_tasks = [
    ["01", "quickstart/"],
    ["02", "user-guide/"],
    ["03", "workflows/"],
    ["04", "configuration/"],
    ["05", "providers/"],
    ["06", "safety/"],
    ["07", "troubleshooting/"],
    ["08", "reference/"]
  ]
  unless task_cards == expected_tasks
    failures << "#{page}: task router must be #{expected_tasks.inspect}, found #{task_cards.inspect}"
  end

  hrefs = tags(html, "a").filter_map { |tag| attributes(tag)["href"] }
  expected_tasks.map(&:last).each do |href|
    count = hrefs.count(href)
    failures << "#{page}: task/resource target #{href.inspect} must appear once, found #{count}" unless count == 1
  end
end

site_css_path = File.join(site_root, "assets", "site.css")
if File.file?(site_css_path)
  site_css = File.read(site_css_path)
  logo_animation = site_css[/@keyframes heroLogoBreath\b(.*?)@keyframes logoGlowBreath/m, 1].to_s
  if logo_animation.empty?
    failures << "assets/site.css: missing heroLogoBreath and logoGlowBreath keyframes"
  elsif logo_animation.include?("filter:")
    failures << "assets/site.css: heroLogoBreath must not animate the expensive filter property"
  end

  shimmer_rules = site_css.scan(/\.terminal-preview::after\s*\{([^}]*)\}/m).flatten
  if shimmer_rules.any? { |rule| rule.match?(/animation:[^;]*infinite/) }
    failures << "assets/site.css: terminal shimmer must be finite"
  end

  %w[hero-field terminal-stage terminal-signals session-timeline terminal-deck docs-command-palette task-router].each do |class_name|
    failures << "assets/site.css: missing .#{class_name} styles" unless site_css.include?(".#{class_name}")
  end
else
  failures << "assets/site.css: built stylesheet is missing"
end

generated_doc_pattern = %r{\A(?:zh-CN/)?docs/[^/]+/index\.html\z}
html_paths.each do |path|
  page = relative_path(path, site_root)
  next unless page.match?(generated_doc_pattern)

  html = File.read(path)
  sidebar = html[/<aside\b[^>]*class=(?:"[^"]*\bdoc-sidebar\b[^"]*"|'[^']*\bdoc-sidebar\b[^']*')[^>]*>.*?<\/aside>/im]
  unless sidebar
    failures << "#{page}: missing documentation sidebar"
    next
  end

  doc_navigation_tags = tags(sidebar, "details").select do |tag|
    class_token?(attributes(tag), "doc-navigation")
  end
  if doc_navigation_tags.length != 1
    failures << "#{page}: expected one details.doc-navigation, found #{doc_navigation_tags.length}"
  elsif attributes(doc_navigation_tags.first).key?("open")
    failures << "#{page}: documentation navigation must be closed in source HTML for mobile-first disclosure"
  end

  group_titles = tags(sidebar, "h2").select { |tag| class_token?(attributes(tag), "doc-nav-group-title") }
  group_navs = tags(sidebar, "nav").select { |tag| class_token?(attributes(tag), "doc-nav-group-links") }
  if group_titles.length < 2 || group_titles.length != group_navs.length
    failures << "#{page}: grouped documentation navigation must expose matching group titles and labelled nav sections"
  end
  group_navs.each do |tag|
    failures << "#{page}: grouped documentation nav is missing aria-labelledby" if attributes(tag)["aria-labelledby"].to_s.empty?
  end

  current_links = tags(sidebar, "a").select { |tag| attributes(tag)["aria-current"] == "page" }
  if current_links.length == 1
    current_href = attributes(current_links.first)["href"]
    failures << "#{page}: sidebar aria-current link must target the current page with href=\"./\"" unless current_href == "./"
  else
    failures << "#{page}: expected exactly one sidebar link with aria-current=\"page\", found #{current_links.length}"
  end

  brand_links = tags(html, "a").select { |tag| class_token?(attributes(tag), "brand") }
  if brand_links.length != 1
    failures << "#{page}: expected exactly one brand home link, found #{brand_links.length}"
  elsif attributes(brand_links.first)["href"] != "../../"
    failures << "#{page}: brand home link must preserve the page locale with href=\"../../\""
  end

  primary_nav = html[/<nav\b[^>]*aria-label=(?:"[^"]+"|'[^']+')[^>]*>.*?<\/nav>/im]
  if primary_nav
    primary_hrefs = tags(primary_nav, "a").filter_map { |tag| attributes(tag)["href"] }
    slug = page[%r{docs/([^/]+)/index\.html\z}, 1]
    language_href = if page.start_with?("zh-CN/")
                      "../../../docs/#{slug}/"
                    else
                      "../../zh-CN/docs/#{slug}/"
                    end
    ["../../#workflow", "../../#safety", language_href].each do |expected_href|
      unless primary_hrefs.include?(expected_href)
        failures << "#{page}: primary navigation must preserve the page locale with href=#{expected_href.inspect}"
      end
    end
  else
    failures << "#{page}: missing primary navigation"
  end
end

html_paths.each do |path|
  page = relative_path(path, site_root)
  html = File.read(path)
  nav_menu_tags = tags(html, "details").select { |tag| class_token?(attributes(tag), "nav-menu") }
  next if nav_menu_tags.empty?

  if nav_menu_tags.any? { |tag| attributes(tag).key?("open") }
    failures << "#{page}: primary navigation must be closed in source HTML for mobile-first disclosure"
  end
end

not_found_path = File.join(site_root, "404.html")
if File.file?(not_found_path)
  not_found_html = File.read(not_found_path)
  local_assets = tags(not_found_html, "link").filter_map { |tag| attributes(tag)["href"] } +
                 tags(not_found_html, "script").filter_map { |tag| attributes(tag)["src"] } +
                 tags(not_found_html, "img").filter_map { |tag| attributes(tag)["src"] }
  invalid_assets = local_assets.reject do |url|
    url.start_with?("http://", "https://", "//", "data:") || url.start_with?("/assets/")
  end
  invalid_assets.each do |url|
    failures << "404.html: local resource #{url.inspect} must use the custom-domain /assets/ prefix"
  end

  home_links = not_found_html.scan(/(<a\b[^>]*>)\s*Go home\s*<\/a>/im).flatten
  if home_links.empty?
    failures << "404.html: missing Go home link"
  elsif home_links.none? { |tag| attributes(tag)["href"] == "/" }
    failures << "404.html: Go home link must use href=\"/\""
  end
  unless tags(not_found_html, "a").any? { |tag| attributes(tag)["href"] == "/docs/" }
    failures << "404.html: missing depth-safe docs recovery link"
  end
else
  failures << "404.html: built page is missing"
end

search_path = File.join(site_root, "search.json")
search_items = []
if File.file?(search_path)
  begin
    parsed_search = JSON.parse(File.read(search_path))
    if parsed_search.is_a?(Array)
      search_items = parsed_search
    else
      failures << "search.json: top-level value must be an array"
    end
  rescue JSON::ParserError => error
    failures << "search.json: invalid JSON: #{error.message}"
  end
else
  failures << "search.json: built search index is missing"
end

failures << "search.json: search index contains no items" if search_items.empty?

duplicate_search_keys = Hash.new { |hash, key| hash[key] = [] }
section_items_by_locale = Hash.new(0)
search_items.each_with_index do |item, index|
  unless item.is_a?(Hash)
    failures << "search.json: item #{index} must be an object"
    next
  end

  locale = item["locale"].to_s.strip
  url = item["url"].to_s.strip
  title = item["title"].to_s.strip
  failures << "search.json: item #{index} has an empty locale" if locale.empty?
  failures << "search.json: item #{index} has an empty url" if url.empty?
  failures << "search.json: item #{index} has an empty title" if title.empty?
  duplicate_search_keys[[locale, url, title]] << index

  page_url, fragment = url.split("#", 2)
  next if fragment.nil? || fragment.empty?

  section_items_by_locale[locale] += 1
  target_path = if page_url.end_with?("/")
                  File.join(site_root, page_url, "index.html")
                else
                  File.join(site_root, page_url)
                end
  unless File.file?(target_path)
    failures << "search.json: item #{index} anchor target page is missing: #{page_url.inspect}"
    next
  end

  decoded_fragment = URI::DEFAULT_PARSER.unescape(fragment)
  target_ids = all_tags(File.read(target_path)).filter_map { |tag| attributes(tag)["id"] }
  unless target_ids.include?(decoded_fragment)
    failures << "search.json: item #{index} anchor ##{decoded_fragment} is missing from #{page_url}"
  end
end

duplicate_search_keys.each do |(locale, url, title), indexes|
  next unless indexes.length > 1

  failures << "search.json: duplicate locale+url+title at items #{indexes.join(', ')}: #{[locale, url, title].inspect}"
end

["en", "zh-CN"].each do |locale|
  if section_items_by_locale[locale].zero?
    failures << "search.json: locale #{locale.inspect} has no section item with an anchor URL"
  end
end

unless failures.empty?
  warn failures.join("\n")
  exit 1
end

puts "site structure check passed (#{html_paths.length} HTML pages, #{search_items.length} search items)"
