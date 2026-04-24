---
name: Wolfram Alpha
description: Computational queries — mate, ciencia, conversiones, datos.
requires:
  bins: []
  env: [WOLFRAM_APP_ID]
---

# Wolfram Alpha

Use cuando necesites una **respuesta computacional precisa**: conversiones de
unidad, cálculos matemáticos, propiedades químicas, datos astronómicos,
historia/geografía con data verificable. El LLM tiende a alucinar en esos
casos — Wolfram no.

## Tools
- `wolfram_short(input, units?)` — single-line answer. "distance earth to moon", "sqrt(2)", "capital of Colombia"
- `wolfram_query(input, format?, units?)` — full `queryresult` con pods (input, result, alternative forms, plots)

## Setup
Registrate en [developer.wolframalpha.com](https://developer.wolframalpha.com/) — AppID free tier 2000 queries/mes.
