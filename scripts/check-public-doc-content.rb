#!/usr/bin/env ruby
# frozen_string_literal: true

require "json"
require "optparse"
require_relative "ruby-compat"
require_relative "public-doc-metrics"

module PublicDocContent
  POLICY_PATH = "dev/docs/public-documentation-content-policy.json"
  GENERATOR_PATH = "scripts/build-docs-site.rb"
  README_WORD_LIMIT = 900
  README_PRIMARY_ITEM_LIMIT = 8
  HOMEPAGE_FEATURE_LIMIT = 4
  HUB_TASK_LIMIT = 8
  REQUIRED_TOPIC_MIN_WORDS = 4
  EXPECTED_ROOT_READMES = ["README.md", "README.zh-CN.md"].freeze
  EXPECTED_LOCALE_ROOTS = { "en" => "docs/en", "zh-CN" => "docs/zh-CN" }.freeze
  EXPECTED_PAGE_FILES = %w[
    README.md quickstart.md installation.md visual-tour.md workflows.md cookbook.md user-guide.md
    safety.md configuration.md permissions-and-sandbox.md appearance.md advanced-configuration.md
    configuration-reference.md providers.md provider-deepseek.md provider-openai-compatible.md
    provider-openai-responses.md provider-anthropic.md provider-gemini.md privacy.md troubleshooting.md
    reference.md mcp.md terminal-compatibility.md status.md changelog.md
  ].freeze
  EXPECTED_PAGE_SLUGS = %w[
    overview quickstart installation visual-tour workflows cookbook user-guide safety configuration
    permissions-and-sandbox appearance advanced-configuration configuration-reference providers
    provider-deepseek provider-openai-compatible provider-openai-responses provider-anthropic
    provider-gemini privacy troubleshooting reference mcp terminal-compatibility status changelog
  ].freeze
  EXPECTED_ENTRY_HTML = [
    "site/index.html",
    "site/docs/index.html",
    "site/zh-CN/index.html",
    "site/zh-CN/docs/index.html",
    "site/404.html"
  ].freeze
  EXPECTED_CONFIG_EXAMPLES = [
    "docs/examples/config/README.md",
    "docs/examples/config/anthropic.toml",
    "docs/examples/config/code-intelligence-rust.toml",
    "docs/examples/config/deepseek-basic.toml",
    "docs/examples/config/gemini.toml",
    "docs/examples/config/mcp-safe-defaults.toml",
    "docs/examples/config/openai-compatible.toml",
    "docs/examples/config/openai-responses.toml"
  ].freeze
  EXPECTED_REQUIRED_TOPICS = {
    "safety" => %w[approval-risk-model],
    "permissions-and-sandbox" => %w[network-control external-directory sandbox-limit],
    "advanced-configuration" => %w[task memory skills-agents compaction code-intelligence terminal model-request-env plugins mcp],
    "privacy" => %w[credentials-plaintext session-log-local data-egress]
  }.freeze
  EXPECTED_SEARCH_FIRST_RESULT = {
    "en" => {
      "install" => "installation", "provider" => "providers", "approval" => "safety",
      "sandbox" => "permissions-and-sandbox", "MCP" => "mcp", "session restore" => "user-guide"
    },
    "zh-CN" => {
      "安装" => "installation", "provider" => "providers", "审批" => "safety",
      "沙箱" => "permissions-and-sandbox", "MCP" => "mcp", "会话恢复" => "user-guide"
    }
  }.freeze
  EXPECTED_ROOT_README_CONTRACT = [
    {
      "path" => "README.md", "locale" => "en", "role" => "repository-entry",
      "cta_key" => "recommended-install-and-docs"
    },
    {
      "path" => "README.zh-CN.md", "locale" => "zh-CN", "role" => "repository-entry",
      "cta_key" => "recommended-install-and-docs"
    }
  ].freeze
  EXPECTED_TOPIC_AUTHORITIES = {
    "installation-release" => "installation", "first-success" => "quickstart",
    "daily-tui" => "user-guide", "commands-keys-paths" => "reference",
    "configuration-fields" => "configuration-reference",
    "permission-network-sandbox" => "permissions-and-sandbox", "risk-model" => "safety",
    "data-and-credentials" => "privacy", "provider-selection" => "providers", "mcp" => "mcp",
    "symptom-to-action" => "troubleshooting", "maturity-and-limits" => "status",
    "release-history" => "changelog"
  }.freeze
  EXPECTED_POLICY_KEYS = %w[
    schema_version purpose root_readmes locale_roots pages hub_task_targets entry_html config_examples
    topic_authorities search_first_result allowlist
  ].freeze

  Violation = Struct.new(:path, :line, :rule, :message, :authority) do
    def to_s
      "#{path}:#{line}: [#{rule}] #{message}; authoritative source: #{authority}"
    end
  end

  INTERNAL_PATTERNS = [
    /\bcrates?\b/i,
    /\bfrontiers?\b/i,
    /\breceipts?[- ]implementations?\b/i,
    /\beval(?:uation)?[- ]fixtures?\b/i,
    /\bmodules?[- ](?:responsibilit(?:y|ies)|ownership)\b/i,
    /\b(?:adapter|projection)s?\b/i,
    /\bCAS\b/,
    /\bprovider\s+crates?\b/i,
    /\b(?:sigil-)?kernel\b/i,
    /\bruntime\s+registry\b/i,
    /\b(?:protocol\s+)?DTOs?\b/,
    /\bContext\s+V[01]\b/i,
    /\bsession\s+projections?\b/i,
    /\b(?:verification|execution|tool)\s+receipts?\b/i,
    /\b(?:dispatch|source|queue|rejection)\s+CAS\b/i,
    /\bstale[- ]frontier\b/i,
    /\bfrozen[- ]requests?\b/i,
    /\bexact\s+(?:proof|admission|economics)\b/i,
    /\bcontrol\s+records?\b/i,
    /\boutput[- ]item\s+arrays?\b/i,
    /\bprovider\s+transport\b/i,
    /\bsource\s+provenance\b/i,
    /\bprovider[- ]neutral\s+chunks?\b/i,
    /\bstream\s+framing\b/i,
    /\beval(?:uation)?\s+harness\b/i,
    /\blifecycle\s+events?\b/i,
    /\bstate\s+machines?\b/i,
    /\b(?:request|session)[- ]owned\b/i,
    /\b(?:compaction|dispatch|tool)\s+barriers?\b/i,
    /\bTree[- ]sitter\s+adapters?\b/i,
    /\btransport\s+lifecycle\b/i
  ].freeze

  INSTALL_COMMANDS = [
    /npm\s+install\s+-g\s+@sigil-ai\/sigil@alpha/i,
    /brew\s+install\s+\S*sigil/i,
    /cargo\s+install\s+--git\b/i,
    /cargo\s+install\s+--path\b/i,
    /npm\s+uninstall\s+-g\s+@sigil-ai\/sigil/i,
    /brew\s+uninstall\s+sigil-ai/i,
    /cargo\s+uninstall\s+sigil\b/i
  ].freeze

  PROVIDER_AUTH = /\bSIGIL_(?:ANTHROPIC_|GEMINI_|OPENAI_COMPATIBLE_|OPENAI_RESPONSES_)?API_KEY\b/
  RELEASE_VERSION = /(?<![0-9.])v?\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?![0-9.])/i
  KEY_TOKEN = /`[^`]*(?:\b(?:Ctrl|Alt|Shift)-[A-Za-z0-9]+\b|\bF\d{1,2}\b|\bPage(?:Up|Down)\b|\b(?:Enter|Esc|Escape|Tab|Backspace|Delete|Space|Home|End|Insert|Up|Down|Left|Right)\b)[^`]*`/

  module_function

  def run(root:)
    Checker.new(File.expand_path(root)).run
  end

  class Checker
    def initialize(root)
      @root = root
      @violations = []
      @policy = nil
    end

    def run
      load_policy
      return sorted_violations unless @policy

      validate_policy_shape
      validate_public_scope
      validate_allowlist_usage
      validate_generator_pages
      validate_roles_and_topics
      validate_config_examples
      validate_internal_language
      validate_budgets_and_routes
      validate_release_versions
      validate_install_authority
      validate_key_matrix_authority
      validate_provider_auth_authority
      sorted_violations
    end

    private

    def load_policy
      @policy = JSON.parse(read(POLICY_PATH))
    rescue Errno::ENOENT
      add(POLICY_PATH, 1, "policy", "content policy is missing", POLICY_PATH)
    rescue JSON::ParserError => error
      add(POLICY_PATH, 1, "policy", "content policy is invalid JSON: #{error.message}", POLICY_PATH)
    end

    def validate_policy_shape
      pages = @policy["pages"]
      unless @policy.keys.sort == EXPECTED_POLICY_KEYS.sort && @policy["schema_version"] == 1 && @policy["purpose"].is_a?(String) && !@policy["purpose"].empty?
        add(POLICY_PATH, 1, "policy-contract", "policy keys, schema version and purpose must match the fixed contract", POLICY_PATH)
      end
      unless pages.is_a?(Array) && pages.length == 26
        add(POLICY_PATH, 1, "policy-pages", "policy must define exactly 26 public page pairs", POLICY_PATH)
      end

      actual_page_files = Array(pages).filter_map { |page| page["file"] }
      unless actual_page_files == EXPECTED_PAGE_FILES
        add(POLICY_PATH, 1, "policy-inventory", "page inventory must match the fixed 26-page public surface", POLICY_PATH)
      end
      actual_page_slugs = Array(pages).filter_map { |page| page["slug"] }
      unless actual_page_slugs == EXPECTED_PAGE_SLUGS
        add(POLICY_PATH, 1, "policy-inventory", "published slugs must match the fixed 26-page route inventory", POLICY_PATH)
      end
      unless @policy["locale_roots"] == EXPECTED_LOCALE_ROOTS
        add(POLICY_PATH, 1, "policy-inventory", "locale roots must match the fixed EN/ZH public surface", POLICY_PATH)
      end
      unless @policy["root_readmes"] == EXPECTED_ROOT_README_CONTRACT
        add(POLICY_PATH, 1, "policy-contract", "root README paths, locales, roles and CTA keys must match the fixed contract", POLICY_PATH)
      end
      unless @policy["entry_html"] == EXPECTED_ENTRY_HTML
        add(POLICY_PATH, 1, "policy-inventory", "hand-authored HTML inventory must match the fixed public surface", POLICY_PATH)
      end
      unless @policy["config_examples"] == EXPECTED_CONFIG_EXAMPLES
        add(POLICY_PATH, 1, "policy-inventory", "configuration example inventory must match the fixed public surface", POLICY_PATH)
      end
      actual_required_topics = Array(pages).each_with_object({}) do |page, result|
        result[page["slug"]] = page["required_topics"] if page.key?("required_topics")
      end
      unless actual_required_topics == EXPECTED_REQUIRED_TOPICS
        add(POLICY_PATH, 1, "policy-contract", "required safety and advanced topic inventory must match the fixed RFC-0022 contract", POLICY_PATH)
      end
      unless @policy["search_first_result"] == EXPECTED_SEARCH_FIRST_RESULT
        add(POLICY_PATH, 1, "policy-contract", "search authority cases must match the fixed EN/ZH query inventory", POLICY_PATH)
      end
      unless @policy["topic_authorities"] == EXPECTED_TOPIC_AUTHORITIES
        add(POLICY_PATH, 1, "policy-contract", "topic authorities must match the fixed single-source contract", POLICY_PATH)
      end

      Array(pages).each do |page|
        missing = %w[slug file role heading_keys heading_labels cta_key cta_target cta_labels].reject { |key| page.key?(key) }
        unless missing.empty?
          add(POLICY_PATH, 1, "policy-contract", "page #{page['slug'] || page['file'] || 'unknown'} is missing #{missing.join(', ')}", POLICY_PATH)
          next
        end

        heading_keys = page["heading_keys"]
        heading_labels = page["heading_labels"]
        valid_keys = heading_keys.is_a?(Array) && !heading_keys.empty? && heading_keys.uniq == heading_keys
        valid_labels = heading_labels.is_a?(Hash) && EXPECTED_LOCALE_ROOTS.keys.all? do |locale|
          heading_labels[locale].is_a?(Array) && heading_labels[locale].length == heading_keys.length
        end
        unless valid_keys && valid_labels
          add(POLICY_PATH, 1, "policy-contract", "page #{page['slug']} heading keys and localized labels must be nonempty and aligned", POLICY_PATH)
        end

        cta_labels = page["cta_labels"]
        valid_cta_labels = cta_labels.is_a?(Hash) && EXPECTED_LOCALE_ROOTS.keys.all? do |locale|
          cta_labels[locale].is_a?(String) && !cta_labels[locale].strip.empty?
        end
        unless valid_cta_labels
          add(POLICY_PATH, 1, "policy-contract", "page #{page['slug']} must define one nonempty CTA label per locale", POLICY_PATH)
        end
      end

      entries = @policy["allowlist"] || []
      entries.each do |entry|
        missing = %w[path rule line_pattern reason].reject { |key| entry[key].is_a?(String) && !entry[key].empty? }
        next if missing.empty?

        add(POLICY_PATH, 1, "allowlist-shape", "allowlist entry is missing #{missing.join(', ')}", POLICY_PATH)
      end
    end

    def validate_public_scope
      expected_files = EXPECTED_PAGE_FILES
      locale_roots.each do |locale, directory|
        actual = Dir.glob(full("#{directory}/*.md")).map { |path| File.basename(path) }.sort
        expected = expected_files.sort
        (expected - actual).each do |file|
          add("#{directory}/#{file}", 1, "locale-pair", "#{locale} public page is missing", POLICY_PATH)
        end
        (actual - expected).each do |file|
          add("#{directory}/#{file}", 1, "locale-pair", "untracked #{locale} public page is outside the 26-page policy", POLICY_PATH)
        end
      end

      required_paths.each do |path|
        add(path, 1, "public-scope", "required public source is missing", POLICY_PATH) unless File.file?(full(path))
      end
    end

    def validate_roles_and_topics
      @policy.fetch("pages", []).each do |page|
        locale_roots.each_value do |directory|
          path = "#{directory}/#{page.fetch('file')}"
          next unless File.file?(full(path))

          source = read(path)
          expected = "<!-- public-doc-role: #{page.fetch('slug')}; authority: #{page.fetch('role')}; sections: #{page.fetch('heading_keys').join(',')}; cta: #{page.fetch('cta_key')} -->"
          role_lines = source.each_line.each_with_index.select { |line, _index| line.include?("public-doc-role:") }
          if role_lines.length != 1 || role_lines.first.first.strip != expected
            line = role_lines.empty? ? 1 : role_lines.first.last + 1
            add(path, line, "page-role", "expected exactly one role marker #{expected}", POLICY_PATH)
          end

          expected_headings = page.fetch("heading_labels").fetch(locale_roots.key(directory))
          actual_headings = source.scan(/^##\s+(.+?)\s*$/).flatten
          unless actual_headings == expected_headings
            add(path, 1, "page-headings", "section headings must match ordered policy keys #{page.fetch('heading_keys').join(', ')}", POLICY_PATH)
          end
          section_bodies(source).each do |heading, body, line|
            next if section_has_content?(body)

            add(path, line, "section-body", "section #{heading} has no user-facing content", page.fetch("slug"))
          end

          validate_page_cta(path, source, page, locale_roots.key(directory))

          Array(page["required_topics"]).each do |topic|
            marker = "<!-- public-doc-topic: #{topic} -->"
            source_lines = source.each_line.to_a
            marker_indexes = source_lines.each_index.select { |index| source_lines[index].include?(marker) }
            if marker_indexes.empty?
              add(path, 1, "required-topic", "required topic #{topic} is missing", page.fetch("slug"))
              next
            end

            if marker_indexes.length > 1
              add(path, marker_indexes[1] + 1, "required-topic", "required topic #{topic} must appear exactly once", page.fetch("slug"))
              next
            end

            marker_index = marker_indexes.first

            unless topic_has_prose?(source_lines, marker_index)
              add(path, marker_index + 1, "required-topic", "required topic #{topic} has no user-facing prose", page.fetch("slug"))
            end
          end
        end
      end
    end

    def validate_config_examples
      path = "docs/examples/config/mcp-safe-defaults.toml"
      return unless File.file?(full(path))

      blocks = read(path).scan(/^\[\[mcp_servers\]\]\s*$.*?(?=^\[\[mcp_servers\]\]\s*$|\z)/m)
      if blocks.empty?
        add(path, 1, "config-example", "MCP example must define at least one server", "docs/en/mcp.md")
        return
      end
      blocks.each do |block|
        next if block.match?(/^transport\s*=\s*"(?:stdio|streamable_http)"\s*$/)

        add(path, line_of(path, "[[mcp_servers]]"), "config-example", "every MCP example server requires an explicit transport", "docs/en/mcp.md")
      end
    end

    def validate_page_cta(path, source, page, locale)
      marker = "<!-- public-doc-cta: #{page.fetch('cta_key')} -->"
      label = page.fetch("cta_labels").fetch(locale)
      target = page.fetch("cta_target")
      prefix = locale == "zh-CN" ? "下一步：" : "Next: "
      punctuation = locale == "zh-CN" ? "。" : "."
      expected_line = "#{prefix}[#{label}](#{target})#{punctuation}"
      nonblank_lines = source.each_line.map(&:strip).reject(&:empty?)
      marker_count = nonblank_lines.count(marker)
      valid_tail = nonblank_lines.last(2) == [marker, expected_line]
      return if marker_count == 1 && valid_tail

      line = source.each_line.find_index { |candidate| candidate.include?("public-doc-cta:") }
      add(path, line ? line + 1 : 1, "page-cta", "final CTA must be exactly #{expected_line}", POLICY_PATH)
    end

    def validate_allowlist_usage
      Array(@policy["allowlist"]).each do |entry|
        next unless %w[path rule line_pattern reason].all? { |key| entry[key].is_a?(String) && !entry[key].empty? }

        path = entry.fetch("path")
        next unless File.file?(full(path))

        pattern = Regexp.new(entry.fetch("line_pattern"))
        matches = read(path).scan(pattern).length
        next if matches == 1

        add(path, 1, "allowlist-precision", "allowlist entry for #{entry.fetch('rule')} must match exactly once; found #{matches}", POLICY_PATH)
      rescue RegexpError => error
        add(POLICY_PATH, 1, "allowlist-shape", "invalid allowlist regex: #{error.message}", POLICY_PATH)
      end
    end

    def validate_generator_pages
      return unless File.file?(full(GENERATOR_PATH))

      source = read(GENERATOR_PATH)
      block = source[/^PAGES\s*=\s*\[(.*?)^\]\.freeze/m, 1].to_s
      actual = block.scan(/^\s*\["([^"]+)",\s*"([^"]+)"/).map { |slug, file| [slug, file] }
      expected = EXPECTED_PAGE_SLUGS.zip(EXPECTED_PAGE_FILES)
      return if actual == expected

      add(GENERATOR_PATH, line_of(GENERATOR_PATH, "PAGES = ["), "generator-pages", "generator page definitions must exactly match the 26 ordered policy pairs", POLICY_PATH)
    end

    def validate_internal_language
      prose_scan_paths.each do |path|
        next unless File.file?(full(path))

        prose_lines(path).each do |line_number, line|
          normalized = line.gsub(/https?:\/\/\S+/i, " ").gsub(/crates\.io/i, " ")
          next unless INTERNAL_PATTERNS.any? { |pattern| normalized.match?(pattern) }
          next if allowlisted?(path, "implementation-language", line)

          add(path, line_number, "implementation-language", "public prose exposes an internal implementation term", "dev/docs/public-documentation-implementation-boundary.md")
        end
      end
    end

    def validate_budgets_and_routes
      metrics = PublicDocMetrics.collect(@root)
      expected_scope = {
        "markdown_sources" => 54,
        "config_example_assets" => EXPECTED_CONFIG_EXAMPLES.length,
        "hand_authored_html" => EXPECTED_ENTRY_HTML.length
      }
      unless metrics.fetch("scope") == expected_scope
        add(POLICY_PATH, 1, "metrics-scope", "metrics scope #{metrics.fetch('scope').inspect} does not match #{expected_scope.inspect}", POLICY_PATH)
      end
      ["README.md", "README.zh-CN.md"].each do |path|
        words = metrics.fetch("markdown").fetch(path).fetch("visible_words")
        if words > README_WORD_LIMIT
          add(path, 1, "readme-budget", "visible prose is #{words} words; limit is #{README_WORD_LIMIT}", path)
        end

        items, line = readme_primary_items(path)
        if items > README_PRIMARY_ITEM_LIMIT
          add(path, line, "readme-primary-items", "primary value list has #{items} items; limit is #{README_PRIMARY_ITEM_LIMIT}", path)
        end
      end

      homepages = ["site/index.html", "site/zh-CN/index.html"]
      feature_counts = homepages.map do |path|
        [path, metrics.fetch("html").fetch(path).fetch("feature_cards")]
      end
      feature_counts.each do |path, count|
        next if count <= HOMEPAGE_FEATURE_LIMIT

        add(path, line_of(path, "feature-grid"), "homepage-feature-budget", "homepage has #{count} feature cards; limit is #{HOMEPAGE_FEATURE_LIMIT}", path)
      end
      if feature_counts.map(&:last).uniq.length > 1
        feature_counts.each do |path, count|
          add(path, line_of(path, "feature-grid"), "homepage-feature-parity", "EN/ZH homepage feature counts differ (#{count})", "site/index.html and site/zh-CN/index.html")
        end
      end

      expected_targets = @policy.fetch("hub_task_targets")
      ["site/docs/index.html", "site/zh-CN/docs/index.html"].each do |path|
        targets = hub_targets(path)
        next if targets == expected_targets && targets.length <= HUB_TASK_LIMIT

        add(path, line_of(path, "task-router"), "hub-task-routes", "task routes must be exactly #{expected_targets.join(', ')} in order; found #{targets.join(', ')}", POLICY_PATH)
      end
    rescue Errno::ENOENT => error
      missing = error.message.sub(/^.* - /, "")
      add(missing, 1, "public-scope", "metrics could not read required source", POLICY_PATH)
    end

    def validate_release_versions
      version_paths = public_paths + [GENERATOR_PATH]
      version_paths.each do |path|
        next unless File.file?(full(path))

        read(path).each_line.each_with_index do |line, index|
          next unless line.match?(RELEASE_VERSION)
          next if path.match?(%r{\Adocs/(?:en|zh-CN)/(?:installation|changelog)\.md\z})
          next if allowlisted?(path, "release-version", line)

          add(path, index + 1, "release-version", "exact release version is outside Installation or Changelog", "installation and changelog")
        end
      end
    end

    def validate_install_authority
      public_paths.each do |path|
        next unless File.file?(full(path))

        occurrences = 0
        read(path).each_line.each_with_index do |line, index|
          matches = INSTALL_COMMANDS.sum { |pattern| line.scan(pattern).length }
          matches.times do
            occurrences += 1
            next if path.match?(%r{\Adocs/(?:en|zh-CN)/installation\.md\z})
            next if allowlisted?(path, "installation-authority", line) && occurrences == 1

            add(path, index + 1, "installation-authority", "install or uninstall command is duplicated outside Installation", "installation")
          end
        end
      end
    end

    def validate_key_matrix_authority
      markdown_paths.each do |path|
        next unless File.file?(full(path))
        next if path.match?(%r{\Adocs/(?:en|zh-CN)/reference\.md\z})

        lines = read(path).each_line.to_a
        index = 0
        while index < lines.length
          unless lines[index].lstrip.start_with?("|")
            index += 1
            next
          end

          start_index = index
          rows = 0
          cursor = index
          while cursor < lines.length && lines[cursor].lstrip.start_with?("|")
            candidate = lines[cursor]
            rows += 1 if candidate.match?(KEY_TOKEN) && !candidate.match?(/^\s*\|?\s*:?-+/)
            cursor += 1
          end
          if rows >= 4
            add(path, start_index + 1, "key-matrix-authority", "shortcut matrix has #{rows} rows outside Reference", "reference")
          end
          index = cursor
        end
      end
    end

    def validate_provider_auth_authority
      public_paths.each do |path|
        next unless File.file?(full(path))

        read(path).each_line.each_with_index do |line, index|
          next unless line.match?(PROVIDER_AUTH)
          next if path.match?(%r{\Adocs/(?:en|zh-CN)/(?:providers|provider-[^/]+)\.md\z})
          next if allowlisted?(path, "provider-auth-authority", line)

          add(path, index + 1, "provider-auth-authority", "provider credential name is duplicated outside provider guidance", "providers")
        end
      end
    end

    def topic_has_prose?(lines, marker_index)
      topic_lines = []
      lines[(marker_index + 1)..-1].to_a.each do |line|
        break if line.include?("public-doc-topic:") || line.match?(/^##\s+/)
        next if line.strip.empty? || line.lstrip.start_with?("<!--") || line.lstrip.start_with?("#")
        next if line.lstrip.start_with?("```") || line.lstrip.start_with?("~~~")

        topic_lines << line
      end
      visible = PublicDocMetrics.visible_markdown(topic_lines.join)
      PublicDocMetrics.word_count(visible) >= REQUIRED_TOPIC_MIN_WORDS
    end

    def section_bodies(source)
      lines = source.each_line.to_a
      headings = lines.each_index.select { |index| lines[index].match?(/^##\s+/) }
      headings.map.with_index do |line_index, index|
        next_index = headings[index + 1] || lines.length
        heading = lines[line_index].sub(/^##\s+/, "").strip
        [heading, lines[(line_index + 1)...next_index], line_index + 1]
      end
    end

    def section_has_content?(lines)
      lines.any? do |line|
        stripped = line.strip
        !stripped.empty? &&
          !stripped.start_with?("<!--") &&
          !stripped.start_with?("```", "~~~")
      end
    end

    def readme_primary_items(path)
      lines = read(path).each_line.to_a
      current_heading = nil
      indexes = []
      lines.each_with_index do |line, index|
        current_heading = line.sub(/^##\s+/, "").strip if line.match?(/^##\s+/)
        next unless line.match?(/^\s{0,3}[-*+]\s+/)
        next if ["Documentation", "文档"].include?(current_heading)

        indexes << index
      end
      [indexes.length, indexes.empty? ? 1 : indexes.first + 1]
    end

    def hub_targets(path)
      read(path).scan(/<a\b[^>]*>/mi).map do |tag|
        next unless tag.match?(/class="[^"]*\btask-card\b[^"]*"/i)

        href = tag[/href="([^"]+)"/i, 1]
        href && href.sub(%r{/\z}, "").split("/").last
      end.compact
    end

    def prose_lines(path)
      path.end_with?(".html") ? html_prose_lines(path) : markdown_prose_lines(path)
    end

    def markdown_prose_lines(path)
      in_fence = false
      in_comment = false
      result = []
      read(path).each_line.each_with_index do |raw, index|
        stripped = raw.lstrip
        if stripped.start_with?("```", "~~~")
          in_fence = !in_fence
          next
        end
        next if in_fence

        line = raw.dup
        if in_comment
          if line.include?("-->")
            line = line.split("-->", 2).last.to_s
            in_comment = false
          else
            next
          end
        end
        while line.include?("<!--")
          before, after = line.split("<!--", 2)
          if after.include?("-->")
            line = before + after.split("-->", 2).last.to_s
          else
            line = before
            in_comment = true
          end
        end
        result << [index + 1, line] unless line.strip.empty?
      end
      result
    end

    def html_prose_lines(path)
      source = read(path)
      sanitized = source.gsub(/<!--.*?-->/m) { |match| mask_preserving_newlines(match) }
      sanitized = sanitized.gsub(/<(script|style|pre)\b[^>]*>.*?<\/\1>/mi) do |match|
        mask_preserving_newlines(match)
      end
      sanitized.each_line.each_with_index.each_with_object([]) do |(raw, index), result|
        line = raw.gsub(/<[^>]+>/, " ")
        result << [index + 1, line] unless line.strip.empty?
      end
    end

    def mask_preserving_newlines(text)
      text.gsub(/[^\n]/, " ")
    end

    def allowlisted?(path, rule, line)
      Array(@policy["allowlist"]).any? do |entry|
        next false unless entry["path"] == path && entry["rule"] == rule

        Regexp.new(entry["line_pattern"]).match?(line)
      rescue RegexpError
        false
      end
    end

    def page_files
      EXPECTED_PAGE_FILES
    end

    def locale_roots
      EXPECTED_LOCALE_ROOTS
    end

    def markdown_paths
      EXPECTED_ROOT_READMES +
        locale_roots.values.flat_map { |directory| page_files.map { |file| "#{directory}/#{file}" } }
    end

    def public_paths
      markdown_paths + EXPECTED_ENTRY_HTML + EXPECTED_CONFIG_EXAMPLES
    end

    def prose_scan_paths
      markdown_paths + EXPECTED_ENTRY_HTML + EXPECTED_CONFIG_EXAMPLES
    end

    def required_paths
      public_paths + [GENERATOR_PATH]
    end

    def read(path)
      File.read(full(path), encoding: "UTF-8")
    end

    def full(path)
      File.join(@root, path)
    end

    def line_of(path, needle)
      line = read(path).each_line.each_with_index.find { |text, _index| text.include?(needle) }
      line ? line.last + 1 : 1
    end

    def add(path, line, rule, message, authority)
      @violations << Violation.new(path, line, rule, message, authority)
    end

    def sorted_violations
      @violations.sort_by { |violation| [violation.path, violation.line, violation.rule, violation.message] }
    end
  end
end

if $PROGRAM_NAME == __FILE__
  options = { root: File.expand_path("..", __dir__) }
  OptionParser.new do |parser|
    parser.on("--root PATH") { |path| options[:root] = path }
  end.parse!

  violations = PublicDocContent.run(root: options.fetch(:root))
  if violations.empty?
    puts "public documentation content checks passed"
  else
    warn violations.map(&:to_s).join("\n")
    exit 1
  end
end
