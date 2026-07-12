#!/usr/bin/env ruby
# frozen_string_literal: true

require "date"
require "json"

require_relative "build-docs-site"

site_root = File.expand_path(ARGV.fetch(0) do
  warn "usage: scripts/check-site-metadata.rb <built-site-directory>"
  exit 2
end)

unless Dir.exist?(site_root)
  warn "site artifact directory does not exist: #{site_root}"
  exit 2
end

errors = []
expected_sitemap_dates = {}

LOCALES.each do |locale, locale_config|
  PAGES.each do |slug, file, _title|
    source_path = File.join(REPO_ROOT, locale_config.fetch(:source_dir), file)
    expected_date = source_last_modified(source_path)
    page_path = File.join(site_root, page_url(locale, slug), "index.html")
    page_label = page_path.delete_prefix("#{site_root}/")

    unless File.file?(page_path)
      errors << "#{page_label}: generated page is missing"
      next
    end

    html = File.read(page_path)
    json_ld = html[%r{<script\b[^>]*type=["']application/ld\+json["'][^>]*>(.*?)</script>}im, 1]
    if json_ld.nil?
      errors << "#{page_label}: missing JSON-LD metadata"
    else
      begin
        metadata = JSON.parse(json_ld)
        actual_date = metadata["dateModified"]
        Date.iso8601(actual_date.to_s)
        unless actual_date == expected_date
          errors << "#{page_label}: dateModified #{actual_date.inspect} does not match #{source_path.delete_prefix("#{REPO_ROOT}/")} #{expected_date}"
        end
      rescue JSON::ParserError, Date::Error => error
        errors << "#{page_label}: invalid JSON-LD dateModified: #{error.message}"
      end
    end

    canonical = "#{SITE_URL}/#{page_url(locale, slug)}"
    errors << "#{page_label}: canonical URL is missing" unless html.include?(%(<link rel="canonical" href="#{canonical}">))
    expected_sitemap_dates[canonical] = expected_date
  end
end

{
  "#{SITE_URL}/" => File.join(REPO_ROOT, "site", "index.html"),
  "#{SITE_URL}/zh-CN/" => File.join(REPO_ROOT, "site", "zh-CN", "index.html"),
  "#{SITE_URL}/docs/" => File.join(REPO_ROOT, "site", "docs", "index.html"),
  "#{SITE_URL}/zh-CN/docs/" => File.join(REPO_ROOT, "site", "zh-CN", "docs", "index.html")
}.each do |url, source_path|
  expected_sitemap_dates[url] = source_last_modified(source_path)
end

sitemap_path = File.join(site_root, "sitemap.xml")
if File.file?(sitemap_path)
  sitemap_entries = File.read(sitemap_path).scan(%r{<url>\s*<loc>([^<]+)</loc>\s*<lastmod>([^<]+)</lastmod>}m).to_h
  expected_sitemap_dates.each do |url, expected_date|
    actual_date = sitemap_entries[url]
    if actual_date.nil?
      errors << "sitemap.xml: missing #{url}"
      next
    end
    begin
      Date.iso8601(actual_date)
      errors << "sitemap.xml: #{url} lastmod #{actual_date.inspect} does not match #{expected_date}" unless actual_date == expected_date
    rescue Date::Error => error
      errors << "sitemap.xml: #{url} has invalid lastmod #{actual_date.inspect}: #{error.message}"
    end
  end
else
  errors << "sitemap.xml: built sitemap is missing"
end

if errors.empty?
  puts "site metadata ok (#{expected_sitemap_dates.length} dated URLs)"
else
  warn errors.join("\n")
  exit 1
end
