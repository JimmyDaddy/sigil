#!/usr/bin/env ruby
# frozen_string_literal: true

require "cgi"
require "fileutils"
require "open3"

VIEWPORTS = [
  { width: 390, height: 844, label: "mobile" },
  { width: 1024, height: 768, label: "tablet" },
  { width: 1440, height: 720, label: "desktop" }
].freeze

PAGES = [
  { path: "index.html", kind: "home" },
  { path: "zh-CN/index.html", kind: "home" },
  { path: "docs/index.html", kind: "docs-hub" },
  { path: "zh-CN/docs/index.html", kind: "docs-hub" },
  { path: "docs/quickstart/index.html", kind: "quickstart" },
  { path: "zh-CN/docs/quickstart/index.html", kind: "quickstart" },
  { path: "docs/reference/index.html", kind: "generated-doc" },
  { path: "zh-CN/docs/reference/index.html", kind: "generated-doc" },
  { path: "docs/changelog/index.html", kind: "generated-doc" },
  { path: "zh-CN/docs/changelog/index.html", kind: "generated-doc" },
  { path: "docs/providers/index.html", kind: "generated-doc" },
  { path: "zh-CN/docs/providers/index.html", kind: "generated-doc" }
].freeze

def executable_on_path(name)
  return name if name.include?(File::SEPARATOR) && File.executable?(name)

  ENV.fetch("PATH", "").split(File::PATH_SEPARATOR).each do |directory|
    candidate = File.join(directory, name)
    return candidate if File.file?(candidate) && File.executable?(candidate)
  end
  nil
end

def find_browser
  candidates = []
  candidates << ENV["SIGIL_SITE_BROWSER"] if ENV["SIGIL_SITE_BROWSER"]
  candidates.concat(%w[google-chrome google-chrome-stable chromium chromium-browser])
  candidates.concat([
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium"
  ])
  candidates.filter_map { |candidate| executable_on_path(candidate) }.first
end

def probe_html(viewport)
  frames = PAGES.each_with_index.map do |page, index|
    <<~HTML
      <section class="case">
        <iframe
          src="#{CGI.escapeHTML(page.fetch(:path))}"
          data-kind="#{page.fetch(:kind)}"
          title="#{CGI.escapeHTML(page.fetch(:path))}"
          onload="measure(this, #{index})"
        ></iframe>
      </section>
      <output id="sigil-case-#{index}"></output>
    HTML
  end.join

  <<~HTML
    <!doctype html>
    <html>
      <head>
        <meta charset="utf-8">
        <style>
          html, body { margin: 0; }
          .case { width: #{viewport.fetch(:width)}px; height: #{viewport.fetch(:height)}px; overflow: hidden; }
          iframe { display: block; width: #{viewport.fetch(:width)}px; height: #{viewport.fetch(:height)}px; border: 0; }
          output { display: none; }
        </style>
        <script>
          function rendered(view, element) {
            if (!element) return false;
            const style = view.getComputedStyle(element);
            return style.display !== "none" &&
              style.visibility !== "hidden" &&
              element.getClientRects().length > 0;
          }

          function elementName(element) {
            if (element.id) return `#${element.id}`;
            const className = typeof element.className === "string" ? element.className.trim() : "";
            return className ? `.${className.split(/\\s+/).join(".")}` : element.tagName;
          }

          function measure(frame, index) {
            const result = document.getElementById(`sigil-case-${index}`);
            try {
              const view = frame.contentWindow;
              const doc = frame.contentDocument;
              const root = doc.documentElement;
              const body = doc.body;
              const style = doc.createElement("style");
              style.textContent = "* { animation: none !important; transition: none !important; }";
              doc.head.appendChild(style);

              const clientWidth = root.clientWidth;
              const scrollWidth = Math.max(root.scrollWidth, body.scrollWidth);
              const hasScrollableAncestor = (element) => {
                let parent = element.parentElement;
                while (parent && parent !== body) {
                  const overflowX = view.getComputedStyle(parent).overflowX;
                  if (overflowX === "auto" || overflowX === "scroll") return true;
                  parent = parent.parentElement;
                }
                return false;
              };
              const overflowing = Array.from(body.querySelectorAll("*"))
                .filter((element) => {
                  if (element.matches(".skip-link")) return false;
                  const computed = view.getComputedStyle(element);
                  if (computed.display === "none" || computed.visibility === "hidden") return false;
                  const bounds = element.getBoundingClientRect();
                  return !hasScrollableAncestor(element) &&
                    (bounds.left < -1 || bounds.right > clientWidth + 1);
                })
                .slice(0, 8)
                .map((element) => {
                  const bounds = element.getBoundingClientRect();
                  return `${elementName(element)}:${bounds.left.toFixed(1)}..${bounds.right.toFixed(1)}`;
                });

              const menu = doc.querySelector("details.nav-menu");
              const menuContent = menu && menu.querySelector("nav");
              const menuOpenByDefault = !!menu && menu.hasAttribute("open");
              const menuContentVisibleByDefault = rendered(view, menuContent);
              const articleHeading = doc.querySelector("article.doc-content h1");
              const sidebar = doc.querySelector("aside.doc-sidebar");
              const docNavigation = sidebar && sidebar.querySelector(":scope > details.doc-navigation");
              const docNavigationPanel = docNavigation && docNavigation.querySelector(".doc-navigation-panel");
              const primaryAction = doc.querySelector(".hero-actions .button.primary");
              const codeScript = Array.from(doc.scripts).some((script) => /(?:^|\\/)code\\.js(?:[?#].*)?$/.test(script.src));
              const firstCodeLine = doc.querySelector(".doc-content pre code .code-line:first-child");
              const firstCodeNumber = firstCodeLine && firstCodeLine.querySelector(".code-line-number");
              const firstCodeContent = firstCodeLine && firstCodeLine.querySelector(".code-line-content");
              let menuClosesAfterAnchor = "";
              const samePageMenuLink = menu && menu.querySelector('nav a[href^="#"]');
              if (samePageMenuLink) {
                menu.setAttribute("open", "");
                samePageMenuLink.click();
                menuClosesAfterAnchor = String(!menu.hasAttribute("open"));
              }

              result.dataset.sigilClient = String(clientWidth);
              result.dataset.sigilScroll = String(scrollWidth);
              result.dataset.sigilOverflow = overflowing.join(",");
              result.dataset.sigilMenuExists = String(!!menu);
              result.dataset.sigilMenuOpen = String(menuOpenByDefault);
              result.dataset.sigilMenuContentVisible = String(menuContentVisibleByDefault);
              result.dataset.sigilMenuClosesAfterAnchor = menuClosesAfterAnchor;
              result.dataset.sigilHeadingTop = articleHeading ? articleHeading.getBoundingClientRect().top.toFixed(1) : "";
              result.dataset.sigilSidebarHeight = sidebar ? sidebar.getBoundingClientRect().height.toFixed(1) : "";
              result.dataset.sigilDocNavigationExists = String(!!docNavigation);
              result.dataset.sigilDocNavigationOpen = String(!!docNavigation && docNavigation.hasAttribute("open"));
              result.dataset.sigilDocPanelVisible = String(rendered(view, docNavigationPanel));
              result.dataset.sigilPrimaryBottom = primaryAction ? primaryAction.getBoundingClientRect().bottom.toFixed(1) : "";
              result.dataset.sigilCodeScript = String(codeScript);
              result.dataset.sigilFirstCodeNumber = firstCodeNumber ? firstCodeNumber.textContent.trim() : "";
              result.dataset.sigilFirstCodeContent = firstCodeContent ? firstCodeContent.textContent.trim() : "";
            } catch (error) {
              result.dataset.sigilError = String(error);
            }
          }
        </script>
      </head>
      <body>
        #{frames}
      </body>
    </html>
  HTML
end

def result_tag(stdout, index)
  id = "sigil-case-#{index}"
  stdout[/<output\b(?=[^>]*\bid="#{Regexp.escape(id)}")[^>]*>/m]
end

def result_attribute(tag, name)
  return nil unless tag

  raw = tag[/\b#{Regexp.escape(name)}="([^"]*)"/, 1]
  raw && CGI.unescapeHTML(raw)
end

def numeric_attribute(tag, name)
  value = result_attribute(tag, name)
  Float(value) unless value.nil? || value.empty?
rescue ArgumentError
  nil
end

site_root = File.expand_path(ARGV.fetch(0) do
  warn "usage: scripts/check-site-viewport.rb <built-site-directory>"
  exit 2
end)
unless Dir.exist?(site_root)
  warn "built site directory is missing: #{site_root}"
  exit 2
end

browser = find_browser
unless browser
  warn "viewport check requires Chrome or Chromium; set SIGIL_SITE_BROWSER to its executable"
  exit 1
end

failures = []
PAGES.each do |page|
  next if File.file?(File.join(site_root, page.fetch(:path)))

  failures << "#{page.fetch(:path)}: built page is missing"
end
unless failures.empty?
  warn failures.join("\n")
  exit 1
end

VIEWPORTS.each do |viewport|
  probe_path = File.join(site_root, ".sigil-viewport-#{Process.pid}-#{viewport.fetch(:width)}.html")
  File.write(probe_path, probe_html(viewport))
  begin
    stdout, stderr, status = Open3.capture3(
      browser,
      "--headless",
      "--disable-gpu",
      "--force-device-scale-factor=1",
      "--hide-scrollbars",
      "--allow-file-access-from-files",
      "--window-size=#{viewport.fetch(:width) + 120},#{viewport.fetch(:height)}",
      "--dump-dom",
      "file://#{probe_path}"
    )
    unless status.success?
      failures << "#{viewport.fetch(:label)}: browser exited #{status.exitstatus}: #{stderr.lines.last(5).join.strip}"
      next
    end

    PAGES.each_with_index do |page, index|
      page_label = "#{page.fetch(:path)} at #{viewport.fetch(:width)}px"
      tag = result_tag(stdout, index)
      unless tag
        failures << "#{page_label}: browser did not emit viewport measurements"
        next
      end

      browser_error = result_attribute(tag, "data-sigil-error")
      client_width = numeric_attribute(tag, "data-sigil-client")&.to_i
      scroll_width = numeric_attribute(tag, "data-sigil-scroll")&.to_i
      overflowing = result_attribute(tag, "data-sigil-overflow").to_s
      if browser_error
        failures << "#{page_label}: viewport probe failed: #{browser_error}"
        next
      end
      if client_width.nil? || scroll_width.nil?
        failures << "#{page_label}: browser emitted incomplete viewport measurements"
        next
      end
      if client_width != viewport.fetch(:width)
        failures << "#{page_label}: expected #{viewport.fetch(:width)}px viewport, browser reported #{client_width}px"
      end
      if scroll_width > client_width
        failures << "#{page_label}: horizontal scroll width #{scroll_width}px exceeds viewport #{client_width}px"
      end
      unless overflowing.empty?
        failures << "#{page_label}: visible content crosses the viewport (#{overflowing})"
      end

      if viewport.fetch(:width) == 390
        unless result_attribute(tag, "data-sigil-menu-exists") == "true"
          failures << "#{page_label}: missing details.nav-menu mobile navigation"
        end
        unless result_attribute(tag, "data-sigil-menu-open") == "false"
          failures << "#{page_label}: details.nav-menu must be closed by default"
        end
        unless result_attribute(tag, "data-sigil-menu-content-visible") == "false"
          failures << "#{page_label}: closed mobile navigation content is still visible"
        end
        if page.fetch(:kind) == "home" && result_attribute(tag, "data-sigil-menu-closes-after-anchor") != "true"
          failures << "#{page_label}: mobile navigation must close after a same-page anchor is activated"
        end
      end

      if viewport.fetch(:width) == 1440
        unless result_attribute(tag, "data-sigil-menu-open") == "true"
          failures << "#{page_label}: desktop details.nav-menu must be open"
        end
        unless result_attribute(tag, "data-sigil-menu-content-visible") == "true"
          failures << "#{page_label}: desktop primary navigation is not visible"
        end
      end

      generated_doc = ["generated-doc", "quickstart"].include?(page.fetch(:kind))
      if viewport.fetch(:width) == 1440 && generated_doc
        unless result_attribute(tag, "data-sigil-doc-navigation-open") == "true"
          failures << "#{page_label}: desktop documentation navigation must be open"
        end
        unless result_attribute(tag, "data-sigil-doc-panel-visible") == "true"
          failures << "#{page_label}: desktop documentation navigation panel is not visible"
        end
      end

      if viewport.fetch(:width) == 390 && generated_doc
        heading_top = numeric_attribute(tag, "data-sigil-heading-top")
        if heading_top.nil?
          failures << "#{page_label}: missing article.doc-content h1"
        elsif heading_top.negative? || heading_top > viewport.fetch(:height) + 120
          failures << "#{page_label}: article h1 starts at #{heading_top}px; expected it within or near the #{viewport.fetch(:height)}px first screen"
        end

        unless result_attribute(tag, "data-sigil-doc-navigation-exists") == "true"
          failures << "#{page_label}: missing aside.doc-sidebar > details.doc-navigation"
        end
        unless result_attribute(tag, "data-sigil-doc-navigation-open") == "false"
          failures << "#{page_label}: documentation navigation must be closed by default"
        end
        unless result_attribute(tag, "data-sigil-doc-panel-visible") == "false"
          failures << "#{page_label}: closed documentation navigation panel is still visible"
        end
        sidebar_height = numeric_attribute(tag, "data-sigil-sidebar-height")
        if sidebar_height.nil?
          failures << "#{page_label}: missing documentation sidebar measurement"
        elsif sidebar_height > viewport.fetch(:height) / 2.0
          failures << "#{page_label}: collapsed documentation sidebar is #{sidebar_height}px tall; expected at most half the first screen"
        end
      end

      if viewport.fetch(:width) == 1440 && page.fetch(:kind) == "home"
        primary_bottom = numeric_attribute(tag, "data-sigil-primary-bottom")
        if primary_bottom.nil?
          failures << "#{page_label}: missing homepage primary action"
        elsif primary_bottom.negative? || primary_bottom > viewport.fetch(:height)
          failures << "#{page_label}: homepage primary action ends at #{primary_bottom}px; expected it within the #{viewport.fetch(:height)}px first screen"
        end
      end

      next unless viewport.fetch(:width) == 390 && page.fetch(:kind) == "quickstart"
      next unless result_attribute(tag, "data-sigil-code-script") == "true"

      code_number = result_attribute(tag, "data-sigil-first-code-number").to_s
      code_content = result_attribute(tag, "data-sigil-first-code-content").to_s
      failures << "#{page_label}: first generated code line must be numbered 1, got #{code_number.inspect}" unless code_number == "1"
      failures << "#{page_label}: first generated code line must not be blank" if code_content.empty?
    end
  ensure
    FileUtils.rm_f(probe_path)
  end
end

unless failures.empty?
  warn failures.join("\n")
  exit 1
end

dimensions = VIEWPORTS.map { |viewport| "#{viewport.fetch(:width)}x#{viewport.fetch(:height)}" }.join(", ")
puts "site viewport check passed at #{dimensions} with #{File.basename(browser)}"
