#!/usr/bin/env ruby
# frozen_string_literal: true

require "cgi"
require "fileutils"
require "open3"

VIEWPORT_WIDTH = 390
VIEWPORT_HEIGHT = 1200
PAGES = [
  "index.html",
  "zh-CN/index.html",
  "docs/reference/index.html",
  "zh-CN/docs/reference/index.html"
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

def probe_html(page_name)
  <<~HTML
    <!doctype html>
    <html>
      <head>
        <meta charset="utf-8">
        <style>
          html, body { margin: 0; }
          iframe { display: block; width: #{VIEWPORT_WIDTH}px; height: #{VIEWPORT_HEIGHT}px; border: 0; }
        </style>
        <script>
          function measure(frame) {
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
                  const computed = view.getComputedStyle(element);
                  if (computed.display === "none" || computed.visibility === "hidden") return false;
                  const bounds = element.getBoundingClientRect();
                  return !hasScrollableAncestor(element) &&
                    (bounds.left < -1 || bounds.right > clientWidth + 1);
                })
                .slice(0, 8)
                .map((element) => {
                  const bounds = element.getBoundingClientRect();
                  const name = element.id ? `#${element.id}` : element.className || element.tagName;
                  return `${name}:${bounds.left.toFixed(1)}..${bounds.right.toFixed(1)}`;
                });
              document.documentElement.dataset.sigilViewportClient = String(clientWidth);
              document.documentElement.dataset.sigilViewportScroll = String(scrollWidth);
              document.documentElement.dataset.sigilViewportOverflow = overflowing.join(",");
            } catch (error) {
              document.documentElement.dataset.sigilViewportError = String(error);
            }
          }
        </script>
      </head>
      <body>
        <iframe src="#{CGI.escapeHTML(page_name)}" onload="measure(this)"></iframe>
      </body>
    </html>
  HTML
end

site_root = File.expand_path(ARGV.fetch(0) do
  warn "usage: scripts/check-site-viewport.rb <built-site-directory>"
  exit 2
end)
browser = find_browser
unless browser
  warn "viewport check requires Chrome or Chromium; set SIGIL_SITE_BROWSER to its executable"
  exit 1
end

failures = []
PAGES.each do |relative_path|
  page_path = File.join(site_root, relative_path)
  unless File.file?(page_path)
    failures << "#{relative_path}: built page is missing"
    next
  end

  probe_path = File.join(File.dirname(page_path), ".sigil-viewport-#{Process.pid}.html")
  File.write(probe_path, probe_html(File.basename(page_path)))
  begin
    stdout, stderr, status = Open3.capture3(
      browser,
      "--headless",
      "--disable-gpu",
      "--force-device-scale-factor=1",
      "--hide-scrollbars",
      "--allow-file-access-from-files",
      "--window-size=500,#{VIEWPORT_HEIGHT}",
      "--dump-dom",
      "file://#{probe_path}"
    )
    unless status.success?
      failures << "#{relative_path}: browser exited #{status.exitstatus}: #{stderr.lines.last(5).join.strip}"
      next
    end

    browser_error = stdout[/data-sigil-viewport-error="([^"]*)"/, 1]
    client_width = stdout[/data-sigil-viewport-client="(\d+)"/, 1]&.to_i
    scroll_width = stdout[/data-sigil-viewport-scroll="(\d+)"/, 1]&.to_i
    overflowing = CGI.unescapeHTML(stdout[/data-sigil-viewport-overflow="([^"]*)"/, 1].to_s)
    if browser_error
      failures << "#{relative_path}: viewport probe failed: #{CGI.unescapeHTML(browser_error)}"
    elsif client_width.nil? || scroll_width.nil?
      failures << "#{relative_path}: browser did not emit viewport measurements"
    elsif client_width != VIEWPORT_WIDTH
      failures << "#{relative_path}: expected #{VIEWPORT_WIDTH}px viewport, browser reported #{client_width}px"
    elsif scroll_width > client_width
      failures << "#{relative_path}: horizontal scroll width #{scroll_width}px exceeds viewport #{client_width}px"
    elsif !overflowing.empty?
      failures << "#{relative_path}: visible content crosses the viewport (#{overflowing})"
    end
  ensure
    FileUtils.rm_f(probe_path)
  end
end

unless failures.empty?
  warn failures.join("\n")
  exit 1
end

puts "site viewport check passed at #{VIEWPORT_WIDTH}px with #{File.basename(browser)}"
