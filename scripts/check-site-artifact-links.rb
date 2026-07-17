#!/usr/bin/env ruby
# frozen_string_literal: true

require "cgi"

site_dir = File.expand_path(ARGV.fetch(0))

unless Dir.exist?(site_dir)
  warn "site artifact directory does not exist: #{site_dir}"
  exit 1
end

html_files = Dir[File.join(site_dir, "**", "*.html")].sort
errors = []

def external_url?(url)
  url.start_with?(
    "http://",
    "https://",
    "mailto:",
    "tel:",
    "data:",
    "javascript:"
  )
end

def target_file_for(site_dir, source_file, path)
  if path.empty?
    source_file
  elsif path.start_with?("/")
    File.join(site_dir, path.sub(%r{\A/+}, ""))
  else
    File.expand_path(path, File.dirname(source_file))
  end
end

def html_ids(path)
  return [] unless File.file?(path) && File.extname(path) == ".html"

  File.read(path).scan(/\bid=["']([^"']+)["']/i).flatten
end

html_files.each do |source_file|
  source = File.read(source_file)
  source.scan(/\b(?:href|src)=["']([^"']+)["']/i).flatten.each do |raw_url|
    url = CGI.unescapeHTML(raw_url)
    next if url.empty? || external_url?(url)

    path, separator, fragment = url.partition("#")
    fragment = nil if separator.empty?
    path = path.partition("?").first
    target = target_file_for(site_dir, source_file, path)
    target = File.join(target, "index.html") if url.end_with?("/") || File.directory?(target)

    unless File.file?(target)
      errors << "#{source_file.sub("#{site_dir}/", "")}: missing local target #{url}"
      next
    end

    next if fragment.nil? || fragment.empty? || File.extname(target) != ".html"

    fragment = CGI.unescape(fragment)
    next if html_ids(target).include?(fragment)

    errors << "#{source_file.sub("#{site_dir}/", "")}: missing anchor ##{fragment} in #{target.sub("#{site_dir}/", "")}"
  end
end

if errors.any?
  warn errors.join("\n")
  exit 1
end

puts "site artifact links ok"
