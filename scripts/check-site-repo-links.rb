#!/usr/bin/env ruby
# frozen_string_literal: true

require "cgi"

REPO_ROOT = File.expand_path("..", __dir__)
SITE_DIR = File.expand_path(ARGV.fetch(0), REPO_ROOT)
REPO_BLOB_PREFIX = "https://github.com/JimmyDaddy/sigil/blob/main/"

unless Dir.exist?(SITE_DIR)
  warn "site artifact directory does not exist: #{SITE_DIR}"
  exit 1
end

errors = []
Dir[File.join(SITE_DIR, "**", "*.html")].sort.each do |html_file|
  source = File.read(html_file)
  source.scan(/\b(?:href|src)=["']([^"']+)["']/i).flatten.each do |raw_url|
    url = CGI.unescapeHTML(raw_url)
    next unless url.start_with?(REPO_BLOB_PREFIX)

    repo_path = url.delete_prefix(REPO_BLOB_PREFIX).split(/[?#]/, 2).first
    repo_path = CGI.unescape(repo_path)
    next if File.exist?(File.join(REPO_ROOT, repo_path))

    relative_file = html_file.sub("#{SITE_DIR}/", "")
    errors << "#{relative_file}: missing repository target #{repo_path}"
  end
end

if errors.any?
  warn errors.join("\n")
  exit 1
end

puts "site repository links ok"
