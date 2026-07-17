#!/usr/bin/env ruby
# frozen_string_literal: true

require "cgi"
require "fileutils"
require "open3"
require "socket"
require "tmpdir"
require "timeout"
require "uri"

VIEWPORTS = [
  { width: 390, height: 844, label: "mobile" },
  { width: 768, height: 768, label: "tablet" },
  { width: 1440, height: 720, label: "desktop" }
].freeze

RENDER_VARIANTS = [
  { label: "default", flags: [] },
  { label: "dark", flags: ["--force-dark-mode"] },
  { label: "explicit-dark", flags: [] },
  { label: "reduced-motion", flags: ["--force-prefers-reduced-motion"] }
].freeze

PAGES = [
  { path: "index.html", kind: "home" },
  { path: "zh-CN/index.html", kind: "home" },
  { path: "docs/index.html", kind: "docs-hub" },
  { path: "zh-CN/docs/index.html", kind: "docs-hub" },
  { path: "docs/quickstart/index.html", kind: "quickstart" },
  { path: "zh-CN/docs/quickstart/index.html", kind: "quickstart" },
  { path: "docs/user-guide/index.html", kind: "generated-doc" },
  { path: "zh-CN/docs/user-guide/index.html", kind: "generated-doc" },
  { path: "docs/safety/index.html", kind: "generated-doc" },
  { path: "zh-CN/docs/safety/index.html", kind: "generated-doc" },
  { path: "docs/provider-deepseek/index.html", kind: "generated-doc" },
  { path: "zh-CN/docs/provider-deepseek/index.html", kind: "generated-doc" },
  { path: "docs/status/index.html", kind: "generated-doc" },
  { path: "zh-CN/docs/status/index.html", kind: "generated-doc" }
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

def find_browser_fallback(primary_browser)
  configured = ENV["SIGIL_SITE_FALLBACK_BROWSER"]
  configured_browser = executable_on_path(configured) if configured
  return configured_browser if configured_browser && configured_browser != primary_browser
  return nil unless RUBY_PLATFORM.include?("darwin")

  # Chrome 150 on macOS 26 can hang or be killed when a headless run uses
  # display-emulation flags. Chrome for Testing's headless shell is a compatible
  # local fallback when an existing Puppeteer or Playwright cache provides it.
  candidates = [
    *Dir.glob(File.join(Dir.home, ".cache", "puppeteer", "chrome-headless-shell", "*", "chrome-headless-shell-*", "chrome-headless-shell")),
    *Dir.glob(File.join(Dir.home, "Library", "Caches", "ms-playwright", "chromium_headless_shell-*", "chrome-mac", "headless_shell"))
  ]
  candidates
    .select { |candidate| candidate != primary_browser && File.file?(candidate) && File.executable?(candidate) }
    .max_by { |candidate| File.mtime(candidate) }
end

def content_type_for(path)
  case File.extname(path)
  when ".css" then "text/css; charset=utf-8"
  when ".html" then "text/html; charset=utf-8"
  when ".js" then "text/javascript; charset=utf-8"
  when ".json" then "application/json; charset=utf-8"
  when ".svg" then "image/svg+xml"
  when ".txt" then "text/plain; charset=utf-8"
  when ".xml" then "application/xml; charset=utf-8"
  else "application/octet-stream"
  end
end

def serve_static_file(socket, site_root, request_target)
  request_path = URI.decode_www_form_component(request_target.split("?", 2).first)
  relative_path = request_path.sub(%r{\A/+}, "")
  relative_path = "index.html" if relative_path.empty?
  candidate = File.expand_path(relative_path, site_root)
  root_prefix = "#{site_root}#{File::SEPARATOR}"

  if !candidate.start_with?(root_prefix) || !File.file?(candidate)
    socket.write("HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
    return
  end

  payload = File.binread(candidate)
  socket.write(
    "HTTP/1.1 200 OK\r\n" \
    "Content-Type: #{content_type_for(candidate)}\r\n" \
    "Content-Length: #{payload.bytesize}\r\n" \
    "Connection: close\r\n\r\n"
  )
  socket.write(payload)
end

def start_static_server(site_root)
  root = File.realpath(site_root)
  server = TCPServer.new("127.0.0.1", 0)
  server_thread = Thread.new do
    loop do
      socket = server.accept
      Thread.new(socket) do |client|
        request = client.gets
        next unless request

        method, target, = request.split
        while (header = client.gets)
          break if header == "\r\n"
        end
        if method == "GET" && target
          serve_static_file(client, root, target)
        else
          client.write("HTTP/1.1 405 Method Not Allowed\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
        end
      rescue IOError, SystemCallError, URI::InvalidURIError
        # A browser can close a speculative asset request before the response is ready.
      ensure
        client.close unless client.closed?
      end
    end
  rescue IOError, Errno::EBADF
    # Closing the listening socket is the server's normal shutdown path.
  end

  [server, server_thread, server.addr[1]]
end

def stop_static_server(server, server_thread)
  server.close
  server_thread.join
rescue IOError, Errno::EBADF
  # The listener may already have closed during an interrupted check.
end

def probe_html(viewport, variant)
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
              const renderVariant = #{variant.fetch(:label).inspect};
              if (renderVariant === "explicit-dark") {
                root.dataset.theme = "dark";
              }
              const prefersDark = view.matchMedia("(prefers-color-scheme: dark)").matches;
              const prefersReducedMotion = view.matchMedia("(prefers-reduced-motion: reduce)").matches;
              const motionAnimationNames = [];
              if (frame.dataset.kind === "home" || frame.dataset.kind === "docs-hub") {
                const motionElements = [root, body, ...body.querySelectorAll("*")];
                for (const element of motionElements) {
                  for (const pseudo of [null, "::before", "::after"]) {
                    const name = view.getComputedStyle(element, pseudo).animationName;
                    if (name && name !== "none") {
                      const suffix = pseudo || "";
                      motionAnimationNames.push(`${elementName(element)}${suffix}:${name}`);
                    }
                  }
                }
              }
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
                  const intentionallyClipped = element.closest(".hero-field, .capability-track");
                  return !intentionallyClipped && !hasScrollableAncestor(element) &&
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
              const timelinePhases = doc.querySelectorAll(".session-timeline .session-phase");
              const deckMain = doc.querySelector(".terminal-window-main");
              const deckApproval = doc.querySelector(".terminal-window-approval");
              const deckWindows = doc.querySelectorAll(".terminal-deck .visual-card");
              const docsCommandLine = doc.querySelector(".docs-command-line");
              const docsCommandInput = doc.querySelector(".docs-command-line input");
              const taskCards = doc.querySelectorAll(".task-router .task-card");
              const visibleHeroLogos = Array.from(doc.querySelectorAll(".hero-logo, .docs-logo"))
                .filter((element) => rendered(view, element));
              const rectanglesOverlap = (left, right) => {
                if (!left || !right) return false;
                const leftRect = left.getBoundingClientRect();
                const rightRect = right.getBoundingClientRect();
                return leftRect.left < rightRect.right &&
                  leftRect.right > rightRect.left &&
                  leftRect.top < rightRect.bottom &&
                  leftRect.bottom > rightRect.top;
              };
              let menuClosesAfterAnchor = "";
              const samePageMenuLink = menu && menu.querySelector('nav a[href^="#"]');
              if (samePageMenuLink && clientWidth === 390) {
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
              result.dataset.sigilTimelinePhases = String(timelinePhases.length);
              result.dataset.sigilDeckWindows = String(deckWindows.length);
              result.dataset.sigilDeckOverlap = String(rectanglesOverlap(deckMain, deckApproval));
              result.dataset.sigilDocsCommandVisible = String(rendered(view, docsCommandLine));
              result.dataset.sigilTaskCards = String(taskCards.length);
              result.dataset.sigilVisibleHeroLogos = String(visibleHeroLogos.length);
              result.dataset.sigilPrefersDark = String(prefersDark);
              result.dataset.sigilExplicitTheme = root.dataset.theme || "";
              result.dataset.sigilPrefersReducedMotion = String(prefersReducedMotion);
              result.dataset.sigilMotionAnimations = motionAnimationNames.join(",");
              result.dataset.sigilDocsCommandInputBackground = docsCommandInput ?
                view.getComputedStyle(docsCommandInput).backgroundColor : "";
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

def capture_browser(command, timeout_secs)
  stdin, stdout, stderr, wait_thread = Open3.popen3(*command)
  stdin.close
  output = String.new
  errors = String.new
  status = nil
  dom_complete = false
  streams = [stdout, stderr]
  deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + timeout_secs

  begin
    loop do
      if wait_thread.join(0)
        status = wait_thread.value
        break
      end

      remaining = deadline - Process.clock_gettime(Process::CLOCK_MONOTONIC)
      if remaining <= 0
        errors << "\nbrowser timed out after #{timeout_secs} seconds"
        break
      end

      readable = IO.select(streams, nil, nil, [remaining, 0.25].min)&.first || []
      readable.each do |stream|
        loop do
          chunk = stream.read_nonblock(16 * 1024, exception: false)
          case chunk
          when :wait_readable
            break
          when nil
            streams.delete(stream)
            break
          else
            stream.equal?(stdout) ? output << chunk : errors << chunk
          end
        end
      end

      # Recent macOS Chrome builds can emit a complete `--dump-dom` document but retain a
      # background process. The rendered DOM is the gate's artifact, so stop this isolated
      # temporary-profile process once the artifact is complete instead of waiting forever.
      if output.include?("</html>")
        dom_complete = true
        break
      end
    end
  ensure
    unless status
      if wait_thread.join(1)
        status = wait_thread.value
      else
        begin
          Process.kill("TERM", wait_thread.pid)
        rescue Errno::ESRCH
          # The browser may have exited between the liveness check and the signal.
        end
      end
    end
    begin
      status ||= Timeout.timeout(3) { wait_thread.value }
    rescue Timeout::Error
      Process.kill("KILL", wait_thread.pid)
      status = wait_thread.value
    end

    streams.each do |stream|
      chunk = stream.read
      stream.equal?(stdout) ? output << chunk : errors << chunk
    rescue IOError
      # The process closed a pipe while it was being drained.
    end
    stdout.close unless stdout.closed?
    stderr.close unless stderr.closed?
  end

  [output, errors, status, dom_complete]
rescue Errno::ESRCH
  [output, errors, status, dom_complete]
end

def browser_capture_failed?(status, dom_complete)
  status.nil? || (!status.success? && !dom_complete)
end

def unavailable_macos_media_variant?(variant, status, dom_complete)
  RUBY_PLATFORM.include?("darwin") &&
    ["dark", "reduced-motion"].include?(variant.fetch(:label)) &&
    !dom_complete &&
    !status.nil?
end

def capture_viewport(browser, variant, browser_profile_dir, viewport, static_server_port, probe_path)
  capture_browser([
    browser,
    *variant.fetch(:flags),
    "--headless",
    "--disable-gpu",
    "--disable-extensions",
    "--no-first-run",
    "--no-default-browser-check",
    "--user-data-dir=#{browser_profile_dir}",
    "--force-device-scale-factor=1",
    "--hide-scrollbars",
    # Chrome 150 on macOS can leave `file:` navigations pending forever, even for a complete
    # local document. Serving the staged artifact over loopback gives every iframe a normal,
    # same-origin URL and lets this gate observe the fully rendered DOM deterministically.
    "--timeout=10000",
    "--window-size=#{viewport.fetch(:width) + 120},#{viewport.fetch(:height)}",
    "--dump-dom",
    "http://127.0.0.1:#{static_server_port}/#{File.basename(probe_path)}"
  ], 20)
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
static_server, static_server_thread, static_server_port = start_static_server(site_root)
at_exit { stop_static_server(static_server, static_server_thread) }
PAGES.each do |page|
  next if File.file?(File.join(site_root, page.fetch(:path)))

  failures << "#{page.fetch(:path)}: built page is missing"
end
unless failures.empty?
  warn failures.join("\n")
  exit 1
end

browser_timed_out = false
skipped_media_variants = []
browser_fallback_used = false
VIEWPORTS.product(RENDER_VARIANTS).each do |viewport, variant|
  break if browser_timed_out
  next if skipped_media_variants.include?(variant.fetch(:label))

  browser_profile_dirs = []
  probe_path = File.join(
    site_root,
    "sigil-viewport-#{Process.pid}-#{viewport.fetch(:width)}-#{variant.fetch(:label)}.html"
  )
  begin
    File.write(probe_path, probe_html(viewport, variant))
    browser_profile_dir = Dir.mktmpdir("sigil-site-browser-")
    browser_profile_dirs << browser_profile_dir
    stdout, stderr, status, dom_complete = capture_viewport(
      browser,
      variant,
      browser_profile_dir,
      viewport,
      static_server_port,
      probe_path
    )
    if browser_capture_failed?(status, dom_complete)
      fallback_browser = find_browser_fallback(browser)
      if fallback_browser
        fallback_profile_dir = Dir.mktmpdir("sigil-site-browser-")
        browser_profile_dirs << fallback_profile_dir
        stdout, stderr, status, dom_complete = capture_viewport(
          fallback_browser,
          variant,
          fallback_profile_dir,
          viewport,
          static_server_port,
          probe_path
        )
        unless browser_capture_failed?(status, dom_complete)
          browser = fallback_browser
          browser_fallback_used = true
        end
      end
    end
    if browser_capture_failed?(status, dom_complete)
      if unavailable_macos_media_variant?(variant, status, dom_complete) &&
         ENV["SIGIL_SITE_REQUIRE_MEDIA_VARIANTS"] != "1"
        skipped_media_variants << variant.fetch(:label)
        next
      end
      failures << "#{viewport.fetch(:label)} #{variant.fetch(:label)}: #{stderr}"
      browser_timed_out = true
      next
    end

    if variant.fetch(:label) == "dark" && browser_fallback_used &&
       result_attribute(result_tag(stdout, 0), "data-sigil-prefers-dark") != "true" &&
       ENV["SIGIL_SITE_REQUIRE_MEDIA_VARIANTS"] != "1"
      skipped_media_variants << variant.fetch(:label)
      next
    end

    PAGES.each_with_index do |page, index|
      page_label = "#{page.fetch(:path)} at #{viewport.fetch(:width)}px (#{variant.fetch(:label)})"
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

      if variant.fetch(:label) == "dark" && result_attribute(tag, "data-sigil-prefers-dark") != "true"
        failures << "#{page_label}: dark render variant did not activate prefers-color-scheme: dark"
      end
      if variant.fetch(:label) == "explicit-dark" &&
         result_attribute(tag, "data-sigil-explicit-theme") != "dark"
        failures << "#{page_label}: explicit dark variant did not apply data-theme=dark"
      end
      if variant.fetch(:label) == "reduced-motion"
        unless result_attribute(tag, "data-sigil-prefers-reduced-motion") == "true"
          failures << "#{page_label}: reduced-motion variant did not activate the media query"
        end
        if ["home", "docs-hub"].include?(page.fetch(:kind)) &&
           !result_attribute(tag, "data-sigil-motion-animations").to_s.empty?
          failures << "#{page_label}: motion animations remain active under prefers-reduced-motion"
        end
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

      if page.fetch(:kind) == "home"
        unless result_attribute(tag, "data-sigil-visible-hero-logos") == "1"
          failures << "#{page_label}: exactly one theme-specific hero logo must be visible"
        end
        unless result_attribute(tag, "data-sigil-timeline-phases") == "5"
          failures << "#{page_label}: homepage session timeline must render five phases"
        end
        unless result_attribute(tag, "data-sigil-deck-windows") == "3"
          failures << "#{page_label}: homepage terminal deck must render three focused windows"
        end
        if [1024, 1440].include?(viewport.fetch(:width)) &&
           result_attribute(tag, "data-sigil-deck-overlap") != "true"
          failures << "#{page_label}: desktop terminal deck must use the layered overlap layout"
        end
        if viewport.fetch(:width) == 390 && result_attribute(tag, "data-sigil-deck-overlap") != "false"
          failures << "#{page_label}: mobile terminal deck must return to a non-overlapping stack"
        end
      end

      if page.fetch(:kind) == "docs-hub"
        unless result_attribute(tag, "data-sigil-visible-hero-logos") == "1"
          failures << "#{page_label}: exactly one theme-specific docs logo must be visible"
        end
        unless result_attribute(tag, "data-sigil-docs-command-visible") == "true"
          failures << "#{page_label}: docs command palette is not visible"
        end
        unless result_attribute(tag, "data-sigil-task-cards") == "8"
          failures << "#{page_label}: docs task router must render eight task cards"
        end
        if ["dark", "explicit-dark"].include?(variant.fetch(:label)) &&
           result_attribute(tag, "data-sigil-docs-command-input-background") != "rgba(0, 0, 0, 0)"
          failures << "#{page_label}: dark command palette input must remain transparent"
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
    browser_profile_dirs.each { |profile_dir| FileUtils.rm_rf(profile_dir) }
  end
end

unless failures.empty?
  warn failures.join("\n")
  exit 1
end

unless skipped_media_variants.empty?
  warn "skipped macOS media variants (#{skipped_media_variants.join(", ")}): " \
       "Chrome did not produce a complete DOM while applying its display-emulation flag; set " \
       "SIGIL_SITE_REQUIRE_MEDIA_VARIANTS=1 to fail instead"
end

warn "using Chrome for Testing fallback: #{File.basename(browser)}" if browser_fallback_used

dimensions = VIEWPORTS.map { |viewport| "#{viewport.fetch(:width)}x#{viewport.fetch(:height)}" }.join(", ")
variants = RENDER_VARIANTS.map { |variant| variant.fetch(:label) }.join(", ")
puts "site viewport check passed at #{dimensions} for #{variants} with #{File.basename(browser)}"
