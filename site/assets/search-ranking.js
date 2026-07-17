((root, factory) => {
  const api = factory();
  if (typeof module === "object" && module.exports) {
    module.exports = api;
  } else {
    root.SigilSearchRanking = api;
  }
})(typeof globalThis === "undefined" ? this : globalThis, () => {
  function normalize(value) {
    return String(value || "").toLowerCase().trim();
  }

  function tokenize(query) {
    return normalize(query)
      .split(/[\s,.;:!?()[\]{}"'`/\\]+/)
      .map((token) => token.trim())
      .filter(Boolean);
  }

  function scoreItem(item, tokens, normalizedQuery) {
    const title = normalize(item.title);
    const section = normalize(item.section);
    const description = normalize(item.description);
    const text = normalize(item.text);
    let score = item.kind === "page" ? 2 : 0;

    if ((item.authority_queries || []).some((query) => normalize(query) === normalizedQuery)) {
      score += 1000;
    }

    for (const token of tokens) {
      let matched = false;
      if (section.includes(token)) {
        score += section.startsWith(token) ? 24 : 18;
        matched = true;
      }
      if (title.includes(token)) {
        score += title.startsWith(token) ? 18 : 12;
        matched = true;
      }
      if (description.includes(token)) {
        score += 7;
        matched = true;
      }
      if (text.includes(token)) {
        score += 2;
        matched = true;
      }
      if (!matched && score < 1000) {
        return 0;
      }
    }

    return score;
  }

  function rank(items, query, locale, maxResults = 8) {
    const normalizedQuery = normalize(query);
    const tokens = tokenize(query);
    if (tokens.length === 0) {
      return [];
    }

    return items
      .filter((item) => item.locale === locale)
      .map((item) => ({ item, score: scoreItem(item, tokens, normalizedQuery) }))
      .filter((entry) => entry.score > 0)
      .sort(
        (left, right) =>
          right.score - left.score ||
          String(left.item.title).localeCompare(String(right.item.title)) ||
          String(left.item.url).localeCompare(String(right.item.url))
      )
      .slice(0, maxResults)
      .map((entry) => entry.item);
  }

  return { normalize, tokenize, scoreItem, rank };
});
