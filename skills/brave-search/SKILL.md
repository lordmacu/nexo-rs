---
name: Brave Search
description: Web search via Brave Search API — JSON results sin el noise de SEO spam.
requires:
  bins: []
  env: [BRAVE_SEARCH_API_KEY]
---

# Brave Search

Use cuando el user quiera buscar en internet: "busca noticias de X", "qué
pasó con Y", "encontrame un artículo sobre Z". Complementa
fetch-url + summarize.

## Tools
- `status`
- `brave_search(query, count?, freshness?, country?, safesearch?)` — `freshness` es `pd|pw|pm|py` (past day/week/month/year)

Returns `{results: [{title, url, description, page_age, language}]}`.

## Setup
API key en [brave.com/search/api](https://brave.com/search/api) (free tier ~2k queries/día).
