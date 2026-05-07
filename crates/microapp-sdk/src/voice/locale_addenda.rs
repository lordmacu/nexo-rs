//! Per-locale system-prompt addenda + voice picker tables.
//!
//! Three pub functions consume [`crate::Locale`] and return
//! `&'static str` content the microapp wires into the LLM turn:
//!
//! - [`voice_mode_addendum`] — SSML-marker tutorial that the
//!   `voice_mode_inbound_transform` plugs into the per-turn
//!   system prompt when voice mode is active for the
//!   conversation. Localised so examples + register match the
//!   operator's chosen locale.
//! - [`language_style_addendum`] — dialect / vocabulary guidance
//!   appended on every turn (independent of voice mode). Locks
//!   the LLM into the configured locale's register (voseo for
//!   `es-AR`, vosotros for `es-ES`, British vocab for `en-GB`,
//!   …).
//! - [`default_voice_for_locale`] — Microsoft Edge voice id
//!   matched to the locale. Falls back to the language-only
//!   canonical, then to the install-wide default.
//!
//! ## Closed-enum exhaustiveness
//!
//! Every locale [`crate::LangCode`] × [`crate::RegionCode`]
//! permutation that the closed enums admit has either an explicit
//! addendum or a documented fallback to a "language-only" or
//! "Latam-neutral" template. The matrix lives in `match` arms so
//! adding a new region without an addendum is a compile-time
//! exhaustiveness error.
//!
//! ## Why `&'static str`
//!
//! Hot path on every inbound turn. Static slices = zero
//! allocation; the addendum body is consumed verbatim by
//! `voice_mode_inbound_transform` / `language_style_inbound_transform`
//! into a JSON `system_addendum` field.

use crate::locale::{LangCode, Locale, RegionCode};
use crate::voice::store::DEFAULT_VOICE_ID;

// ────────────────────────────────────────────────────────────────
// Voice picker
// ────────────────────────────────────────────────────────────────

/// Microsoft Edge voice id matched to the locale.
///
/// Resolution order:
///   1. Exact `(language, Some(region))` match.
///   2. Language-only fallback for the same language.
///   3. [`DEFAULT_VOICE_ID`] (install-wide Latam Spanish default).
///
/// Operator-supplied per-conversation overrides take precedence
/// over this picker — see
/// `agent-creator-microapp/src/voice_mode/tool.rs` reply
/// transform; the picker only fires when the stored voice equals
/// [`DEFAULT_VOICE_ID`].
pub fn default_voice_for_locale(locale: Option<&Locale>) -> &'static str {
    let Some(loc) = locale else {
        return DEFAULT_VOICE_ID;
    };
    match (loc.language(), loc.region()) {
        // Spanish family.
        (LangCode::Es, Some(RegionCode::Ar)) => "es-AR-ElenaNeural",
        (LangCode::Es, Some(RegionCode::Mx)) => "es-MX-DaliaNeural",
        (LangCode::Es, Some(RegionCode::Es)) => "es-ES-ElviraNeural",
        (LangCode::Es, Some(RegionCode::Co)) => "es-CO-SalomeNeural",
        (LangCode::Es, Some(RegionCode::Pe)) => "es-PE-CamilaNeural",
        (LangCode::Es, Some(RegionCode::Cl)) => "es-CL-CatalinaNeural",
        (LangCode::Es, Some(RegionCode::Us)) => "es-US-PalomaNeural",
        (LangCode::Es, _) => "es-MX-DaliaNeural", // language-only / unmapped region
        // English family.
        (LangCode::En, Some(RegionCode::Us)) => "en-US-AriaNeural",
        (LangCode::En, Some(RegionCode::Gb)) => "en-GB-SoniaNeural",
        (LangCode::En, Some(RegionCode::Au)) => "en-AU-NatashaNeural",
        (LangCode::En, Some(RegionCode::Ca)) => "en-CA-ClaraNeural",
        (LangCode::En, _) => "en-US-AriaNeural",
        // Portuguese.
        (LangCode::Pt, Some(RegionCode::Br)) => "pt-BR-FranciscaNeural",
        (LangCode::Pt, Some(RegionCode::Pt)) => "pt-PT-RaquelNeural",
        (LangCode::Pt, _) => "pt-BR-FranciscaNeural",
        // French.
        (LangCode::Fr, Some(RegionCode::Fr)) => "fr-FR-DeniseNeural",
        (LangCode::Fr, Some(RegionCode::Ca)) => "fr-CA-SylvieNeural",
        (LangCode::Fr, _) => "fr-FR-DeniseNeural",
        // Italian.
        (LangCode::It, Some(RegionCode::It)) => "it-IT-ElsaNeural",
        (LangCode::It, _) => "it-IT-ElsaNeural",
        // German.
        (LangCode::De, Some(RegionCode::De)) => "de-DE-KatjaNeural",
        (LangCode::De, _) => "de-DE-KatjaNeural",
        // Japanese.
        (LangCode::Ja, Some(RegionCode::Jp)) => "ja-JP-NanamiNeural",
        (LangCode::Ja, _) => "ja-JP-NanamiNeural",
        // Chinese.
        (LangCode::Zh, Some(RegionCode::Cn)) => "zh-CN-XiaoxiaoNeural",
        (LangCode::Zh, _) => "zh-CN-XiaoxiaoNeural",
    }
}

// ────────────────────────────────────────────────────────────────
// Voice-mode SSML-marker tutorials
// ────────────────────────────────────────────────────────────────

/// SSML-marker tutorial appended to the system prompt when voice
/// mode is active for the conversation. Localised so examples +
/// register match the operator's chosen locale.
///
/// Routing fallthroughs:
///   - `Es` with no region → [`ES_LATAM_NEUTRAL_VOICE_ADDENDUM`]
///     (NOT the legacy voseo template — fixes the bug where
///     `language: "es"` agents inherited Argentine flavour).
///   - `Es` with an unmapped region → same Latam-neutral template.
///   - `En` with no region → [`EN_US_VOICE_ADDENDUM`].
///   - `Pt` with no region → [`PT_BR_VOICE_ADDENDUM`].
///   - Other languages: a single template per language (no
///     regional dialect splits in v1).
///   - `None` → [`ES_LATAM_NEUTRAL_VOICE_ADDENDUM`] (safest
///     default — voice mode is Spanish-leaning today).
pub fn voice_mode_addendum(locale: Option<&Locale>) -> &'static str {
    let Some(loc) = locale else {
        return ES_LATAM_NEUTRAL_VOICE_ADDENDUM;
    };
    match (loc.language(), loc.region()) {
        // Spanish family.
        (LangCode::Es, Some(RegionCode::Ar)) => ES_AR_VOICE_ADDENDUM,
        (LangCode::Es, Some(RegionCode::Es)) => ES_ES_VOICE_ADDENDUM,
        (LangCode::Es, Some(RegionCode::Us)) => ES_US_VOICE_ADDENDUM,
        (LangCode::Es, _) => ES_LATAM_NEUTRAL_VOICE_ADDENDUM,
        // English family.
        (LangCode::En, Some(RegionCode::Gb)) => EN_GB_VOICE_ADDENDUM,
        (LangCode::En, Some(RegionCode::Au)) => EN_AU_VOICE_ADDENDUM,
        (LangCode::En, _) => EN_US_VOICE_ADDENDUM,
        // Portuguese.
        (LangCode::Pt, Some(RegionCode::Pt)) => PT_PT_VOICE_ADDENDUM,
        (LangCode::Pt, _) => PT_BR_VOICE_ADDENDUM,
        // Other languages: one template per language.
        (LangCode::Fr, _) => FR_VOICE_ADDENDUM,
        (LangCode::It, _) => IT_VOICE_ADDENDUM,
        (LangCode::De, _) => DE_VOICE_ADDENDUM,
        (LangCode::Ja, _) => JA_VOICE_ADDENDUM,
        (LangCode::Zh, _) => ZH_VOICE_ADDENDUM,
    }
}

// ────────────────────────────────────────────────────────────────
// Language-style addenda (always-on, not voice-mode dependent)
// ────────────────────────────────────────────────────────────────

/// Per-turn dialect / vocabulary guidance. Returns `None` when
/// the locale is unset — caller skips the system_addendum slot.
pub fn language_style_addendum(locale: Option<&Locale>) -> Option<&'static str> {
    let loc = locale?;
    Some(match (loc.language(), loc.region()) {
        (LangCode::Es, Some(RegionCode::Ar)) => ES_AR_STYLE_ADDENDUM,
        (LangCode::Es, Some(RegionCode::Es)) => ES_ES_STYLE_ADDENDUM,
        (LangCode::Es, Some(RegionCode::Us)) => ES_US_STYLE_ADDENDUM,
        (LangCode::Es, _) => ES_LATAM_NEUTRAL_STYLE_ADDENDUM,
        (LangCode::En, Some(RegionCode::Gb)) => EN_GB_STYLE_ADDENDUM,
        (LangCode::En, Some(RegionCode::Au)) => EN_AU_STYLE_ADDENDUM,
        (LangCode::En, _) => EN_US_STYLE_ADDENDUM,
        (LangCode::Pt, Some(RegionCode::Pt)) => PT_PT_STYLE_ADDENDUM,
        (LangCode::Pt, _) => PT_BR_STYLE_ADDENDUM,
        (LangCode::Fr, _) => FR_STYLE_ADDENDUM,
        (LangCode::It, _) => IT_STYLE_ADDENDUM,
        (LangCode::De, _) => DE_STYLE_ADDENDUM,
        (LangCode::Ja, _) => JA_STYLE_ADDENDUM,
        (LangCode::Zh, _) => ZH_STYLE_ADDENDUM,
    })
}

// ────────────────────────────────────────────────────────────────
// Voice-mode addendum templates — Spanish family
// ────────────────────────────────────────────────────────────────

/// `es-AR` — voseo, ejemplos rioplatenses. Inherits the legacy
/// agent-creator-microapp constant (which was Argentine-flavoured
/// by accident); now opted into explicitly.
pub const ES_AR_VOICE_ADDENDUM: &str = "# MODO VOZ ACTIVO

Esta respuesta se lee en voz alta (TTS de Edge). Pensá como narrador: el \
texto es un guion para audio, no markdown para web. **Markdown como \
`**negrita**` o `_cursiva_` NO afecta el audio** — usá los marcadores de \
abajo para énfasis y cadencia. El sistema los convierte a SSML.

## Marcadores

- `[pause=300ms]` — pausa (50–1500 ms). Útil antes de una idea importante \
  o entre dos frases que necesitan respirar.
- `[em]palabra[/em]` — énfasis moderado en la palabra clave de la oración.
- `[strong]palabra[/strong]` — énfasis fuerte (úsalo poco, sólo en lo \
  realmente crítico).
- `[spell]SAT[/spell]` — letra por letra (códigos, siglas que no son \
  conocidas, placas, IDs).
- `[slow]frase[/slow]` — más lento (datos numéricos largos, instrucciones).
- `[fast]frase[/fast]` — más rápido (paréntesis aclaratorios).

## Cuándo usarlos (con intención)

Marcá la intención semántica de cada respuesta:

- Un dato clave que el usuario debe recordar → `[em]…[/em]`.
- Un dato crítico (deadline, monto, advertencia legal) → `[strong]…[/strong]`.
- Antes de una pregunta que cambia de tema → `[pause=400ms]`.
- Antes de enumerar → `[pause=300ms]`. Después de cada item → `[pause=200ms]`.
- Sigla poco común → `[spell]…[/spell]`.

## Ejemplos antes → después

- `**Artículo 15** protege la intimidad.` → \
  `[strong]Artículo 15[/strong] protege la [em]intimidad[/em].`
- `Tenés 30 días para responder.` → \
  `Tenés [strong]30 días[/strong] para responder.`
- `¿Hay algo específico que te preocupe? Por ejemplo:` → \
  `[pause=300ms] ¿Hay algo [em]específico[/em] que te preocupe? [pause=400ms] Por ejemplo:`
- `La SIC recibe quejas.` → `La [spell]SIC[/spell] recibe quejas.`

## Cosas que NO necesitás envolver

Fechas (`2026-05-05`), monedas (`$50.000`), cifras grandes (`12345`) y \
siglas largas en mayúsculas (3+ letras) ya las pronuncia bien automático.

Los marcadores **se remueven** del texto que el usuario ve en la web; sólo \
afectan el audio. Usalos en cada respuesta — la voz plana sin marcadores \
suena robótica.";

/// `es-MX` / `es-CO` / `es-PE` / `es-CL` / language-only `es` —
/// tuteo, vocabulario neutro Latam. Ejemplos sin voseo.
pub const ES_LATAM_NEUTRAL_VOICE_ADDENDUM: &str = "# MODO VOZ ACTIVO

Esta respuesta se lee en voz alta (TTS de Edge). Piensa como narrador: el \
texto es un guion para audio, no markdown para web. **Markdown como \
`**negrita**` o `_cursiva_` NO afecta el audio** — usa los marcadores de \
abajo para énfasis y cadencia. El sistema los convierte a SSML.

## Marcadores

- `[pause=300ms]` — pausa (50–1500 ms). Útil antes de una idea importante \
  o entre dos frases que necesitan respirar.
- `[em]palabra[/em]` — énfasis moderado en la palabra clave de la oración.
- `[strong]palabra[/strong]` — énfasis fuerte (úsalo poco, sólo en lo \
  realmente crítico).
- `[spell]SAT[/spell]` — letra por letra (códigos, siglas que no son \
  conocidas, placas, IDs).
- `[slow]frase[/slow]` — más lento (datos numéricos largos, instrucciones).
- `[fast]frase[/fast]` — más rápido (paréntesis aclaratorios).

## Cuándo usarlos (con intención)

Marca la intención semántica de cada respuesta:

- Un dato clave que el usuario debe recordar → `[em]…[/em]`.
- Un dato crítico (deadline, monto, advertencia legal) → `[strong]…[/strong]`.
- Antes de una pregunta que cambia de tema → `[pause=400ms]`.
- Antes de enumerar → `[pause=300ms]`. Después de cada item → `[pause=200ms]`.
- Sigla poco común → `[spell]…[/spell]`.

## Ejemplos antes → después

- `**Artículo 15** protege la intimidad.` → \
  `[strong]Artículo 15[/strong] protege la [em]intimidad[/em].`
- `Tienes 30 días para responder.` → \
  `Tienes [strong]30 días[/strong] para responder.`
- `¿Hay algo específico que te preocupe? Por ejemplo:` → \
  `[pause=300ms] ¿Hay algo [em]específico[/em] que te preocupe? [pause=400ms] Por ejemplo:`
- `La SIC recibe quejas.` → `La [spell]SIC[/spell] recibe quejas.`

## Cosas que NO necesitas envolver

Fechas (`2026-05-05`), monedas (`$50.000`), cifras grandes (`12345`) y \
siglas largas en mayúsculas (3+ letras) ya las pronuncia bien automático.

Los marcadores **se remueven** del texto que el usuario ve en la web; sólo \
afectan el audio. Úsalos en cada respuesta — la voz plana sin marcadores \
suena robótica.";

/// `es-ES` — tuteo + castellano peninsular. Permite `vosotros`, \
/// vocabulario castellano (`vale`, `tío`).
pub const ES_ES_VOICE_ADDENDUM: &str = "# MODO VOZ ACTIVO

Esta respuesta se lee en voz alta (TTS de Edge). Piensa como narrador: el \
texto es un guion para audio, no markdown para web. **Markdown como \
`**negrita**` o `_cursiva_` NO afecta el audio** — usa los marcadores de \
abajo para énfasis y cadencia. El sistema los convierte a SSML.

Estás hablando en español de España: tuteo informal, `vosotros` para \
plural, vocabulario castellano natural (`vale`, `genial`).

## Marcadores

- `[pause=300ms]` — pausa (50–1500 ms).
- `[em]palabra[/em]` — énfasis moderado.
- `[strong]palabra[/strong]` — énfasis fuerte (úsalo poco).
- `[spell]SAT[/spell]` — letra por letra (códigos, siglas raras).
- `[slow]frase[/slow]` — más lento.
- `[fast]frase[/fast]` — más rápido.

## Ejemplos antes → después

- `**Artículo 15** protege la intimidad.` → \
  `[strong]Artículo 15[/strong] protege la [em]intimidad[/em].`
- `Tienes 30 días para responder.` → \
  `Tienes [strong]30 días[/strong] para responder.`
- `¿Hay algo específico que os preocupe?` → \
  `[pause=300ms] ¿Hay algo [em]específico[/em] que os preocupe?`

Fechas, monedas, cifras grandes y siglas largas en mayúsculas (3+ letras) \
ya las pronuncia bien automático. Los marcadores se remueven del texto \
escrito; sólo afectan el audio.";

/// `es-US` — Spanish con awareness de contexto bilingüe / \
/// préstamos del inglés permitidos sin traducción forzada.
pub const ES_US_VOICE_ADDENDUM: &str = "# MODO VOZ ACTIVO

Esta respuesta se lee en voz alta (TTS de Edge). Piensa como narrador: el \
texto es un guion para audio, no markdown para web.

Estás hablando con un público hispanohablante en Estados Unidos. Usa \
tuteo neutral; los préstamos del inglés (`email`, `parking`, `shopping`, \
nombres de marcas) **no se traducen** — pronúncialos como en inglés.

## Marcadores

- `[pause=300ms]`, `[em]…[/em]`, `[strong]…[/strong]`, `[spell]…[/spell]`, \
  `[slow]…[/slow]`, `[fast]…[/fast]`.

## Ejemplos antes → después

- `Te enviamos un **email**.` → \
  `Te enviamos un [em]email[/em].`
- `El parking cierra a las 10.` → \
  `El parking cierra a las [strong]10[/strong].`
- `Para más info, busca en Google.` → \
  `Para más info, busca en [spell]Google[/spell].` (sólo si la marca \
  podría malpronunciarse).

Los marcadores se remueven del texto escrito; sólo afectan el audio.";

// ────────────────────────────────────────────────────────────────
// Voice-mode addendum templates — English family
// ────────────────────────────────────────────────────────────────

/// `en-US` / language-only `en` — American spelling and idiom.
pub const EN_US_VOICE_ADDENDUM: &str = "# VOICE MODE ACTIVE

This reply is read aloud (Edge TTS). Think like a narrator: the text is a \
script for audio, not markdown for web. **Markdown like `**bold**` or \
`_italics_` does NOT affect audio** — use the markers below for emphasis \
and cadence. The system converts them to SSML.

You are speaking American English: standard US spelling (`color`, \
`organize`), American vocabulary, neutral register.

## Markers

- `[pause=300ms]` — pause (50–1500 ms).
- `[em]word[/em]` — moderate emphasis.
- `[strong]word[/strong]` — strong emphasis (use sparingly).
- `[spell]SAT[/spell]` — letter by letter (codes, lesser-known acronyms).
- `[slow]phrase[/slow]` — slower (long numeric data, instructions).
- `[fast]phrase[/fast]` — faster (parenthetical asides).

## Before → after examples

- `**Article 15** protects privacy.` → \
  `[strong]Article 15[/strong] protects [em]privacy[/em].`
- `You have 30 days to respond.` → \
  `You have [strong]30 days[/strong] to respond.`

Markers are stripped from the visible transcript; they only affect audio.";

/// `en-GB` — British spelling, British vocab.
pub const EN_GB_VOICE_ADDENDUM: &str = "# VOICE MODE ACTIVE

This reply is read aloud (Edge TTS). Think like a narrator: the text is a \
script for audio, not markdown for web. **Markdown like `**bold**` or \
`_italics_` does NOT affect audio** — use the markers below for emphasis \
and cadence.

You are speaking British English: British spelling (`colour`, \
`organise`, `centre`), British vocabulary (`lift`, `lorry`, `holiday`), \
neutral register.

## Markers

- `[pause=300ms]`, `[em]…[/em]`, `[strong]…[/strong]`, `[spell]…[/spell]`, \
  `[slow]…[/slow]`, `[fast]…[/fast]`.

## Before → after examples

- `**Article 15** protects privacy.` → \
  `[strong]Article 15[/strong] protects [em]privacy[/em].`
- `You have 30 days to respond.` → \
  `You have [strong]30 days[/strong] to respond.`
- `The lift on the ground floor is out of order.` → \
  `The [em]lift[/em] on the ground floor is out of order.`

Markers are stripped from the visible transcript; they only affect audio.";

/// `en-AU` — American spelling acceptable, Australian colour OK \
/// in moderation.
pub const EN_AU_VOICE_ADDENDUM: &str = "# VOICE MODE ACTIVE

This reply is read aloud (Edge TTS). Think like a narrator: the text is a \
script for audio, not markdown for web.

You are speaking Australian English: standard spelling, Australian \
vocabulary acceptable in moderation (`mate`, `arvo`, `g'day`) — keep the \
register neutral and professional unless context invites informality.

## Markers

- `[pause=300ms]`, `[em]…[/em]`, `[strong]…[/strong]`, `[spell]…[/spell]`, \
  `[slow]…[/slow]`, `[fast]…[/fast]`.

Markers are stripped from the visible transcript; they only affect audio.";

// ────────────────────────────────────────────────────────────────
// Voice-mode addendum templates — Portuguese
// ────────────────────────────────────────────────────────────────

/// `pt-BR` / language-only `pt` — português brasileiro.
pub const PT_BR_VOICE_ADDENDUM: &str = "# MODO VOZ ATIVO

Esta resposta é lida em voz alta (TTS Edge). Pense como narrador: o texto \
é um roteiro para áudio, não markdown para web.

Você está falando português brasileiro: `você`, vocabulário neutro, \
registro educado.

## Marcadores

- `[pause=300ms]`, `[em]…[/em]`, `[strong]…[/strong]`, `[spell]…[/spell]`, \
  `[slow]…[/slow]`, `[fast]…[/fast]`.

Os marcadores são removidos do texto visível; só afetam o áudio.";

/// `pt-PT` — português europeu.
pub const PT_PT_VOICE_ADDENDUM: &str = "# MODO VOZ ATIVO

Esta resposta é lida em voz alta (TTS Edge). Pense como narrador: o texto \
é um guião para áudio, não markdown para web.

Está a falar português europeu: `tu` informal, `vós` raro, vocabulário \
de Portugal (`fixe`, `comboio`, `pequeno-almoço`), registo educado.

## Marcadores

- `[pause=300ms]`, `[em]…[/em]`, `[strong]…[/strong]`, `[spell]…[/spell]`, \
  `[slow]…[/slow]`, `[fast]…[/fast]`.

Os marcadores são removidos do texto visível; só afetam o áudio.";

// ────────────────────────────────────────────────────────────────
// Voice-mode addendum templates — Other languages (one per lang)
// ────────────────────────────────────────────────────────────────

/// French (any region — language-only template).
pub const FR_VOICE_ADDENDUM: &str = "# MODE VOCAL ACTIF

Cette réponse est lue à voix haute (TTS Edge). Pensez en narrateur : le \
texte est un script audio, pas du markdown web.

## Marqueurs

- `[pause=300ms]`, `[em]…[/em]`, `[strong]…[/strong]`, `[spell]…[/spell]`, \
  `[slow]…[/slow]`, `[fast]…[/fast]`.

Les marqueurs sont retirés du transcript visible ; ils n'affectent que \
l'audio.";

/// Italian.
pub const IT_VOICE_ADDENDUM: &str = "# MODALITÀ VOCALE ATTIVA

Questa risposta viene letta ad alta voce (TTS Edge). Pensa come un \
narratore: il testo è un copione audio, non markdown web.

## Marcatori

- `[pause=300ms]`, `[em]…[/em]`, `[strong]…[/strong]`, `[spell]…[/spell]`, \
  `[slow]…[/slow]`, `[fast]…[/fast]`.

I marcatori vengono rimossi dal trascritto visibile; influenzano solo \
l'audio.";

/// German.
pub const DE_VOICE_ADDENDUM: &str = "# SPRACHMODUS AKTIV

Diese Antwort wird vorgelesen (Edge TTS). Denke wie ein Erzähler: der \
Text ist ein Skript für Audio, kein Markdown für Web.

## Marker

- `[pause=300ms]`, `[em]…[/em]`, `[strong]…[/strong]`, `[spell]…[/spell]`, \
  `[slow]…[/slow]`, `[fast]…[/fast]`.

Marker werden aus dem sichtbaren Transcript entfernt; sie wirken nur auf \
das Audio.";

/// Japanese.
pub const JA_VOICE_ADDENDUM: &str = "# 音声モード有効

この応答は音声で読み上げられます (Edge TTS)。テキストは Web 向けの \
markdown ではなく音声用の台本です。

## マーカー

- `[pause=300ms]`、`[em]…[/em]`、`[strong]…[/strong]`、`[spell]…[/spell]`、\
  `[slow]…[/slow]`、`[fast]…[/fast]`。

マーカーは表示テキストからは除去され、音声にのみ反映されます。";

/// Chinese (Simplified).
pub const ZH_VOICE_ADDENDUM: &str = "# 语音模式已开启

此回复会以语音朗读 (Edge TTS)。请把文本视为音频脚本，而不是网页 markdown。

## 标记

- `[pause=300ms]`、`[em]…[/em]`、`[strong]…[/strong]`、`[spell]…[/spell]`、\
  `[slow]…[/slow]`、`[fast]…[/fast]`。

标记会从可见转录中移除，仅影响音频。";

// ────────────────────────────────────────────────────────────────
// Language-style addenda — always-on dialect/register guidance
// ────────────────────────────────────────────────────────────────

/// `es-AR` — voseo affirmed.
pub const ES_AR_STYLE_ADDENDUM: &str = "# IDIOMA

Respondé estrictamente en español de Argentina (es-AR).

- Voseo: usá `vos`, `sos`, `tenés`, `podés`, `querés`, `decís`, `andás`, \
  `vení`, `andate`. NO uses `tú` ni `vosotros`.
- Vocabulario rioplatense: `che` con moderación, expresiones porteñas \
  bienvenidas en contexto informal.
- Cuando el usuario diga su nombre y la transcripción podría haber sido \
  ambigua, preferí la grafía nativa del español (Cristian, no Christian; \
  Yésica, no Jessica) y corregila silenciosamente.";

/// `es-MX` / `es-CO` / `es-PE` / `es-CL` / language-only `es` — \
/// tuteo + neutral Latam.
pub const ES_LATAM_NEUTRAL_STYLE_ADDENDUM: &str = "# IDIOMA

Responde estrictamente en español neutral de Latinoamérica (es).

- Tuteo: usa `tú`, `tienes`, `puedes`, `quieres`, `dices`. NO uses voseo \
  (`vos`, `sos`, `tenés`, `podés`, `querés`, `decís`, `andás`).
- Vocabulario neutral entendible en cualquier país hispanohablante; \
  evita regionalismos fuertes (no `che`, no `vale`, no `chido`).
- Cuando el usuario diga su nombre y la transcripción podría haber sido \
  ambigua, prefiere la grafía nativa del español (Cristian, no Christian; \
  Yésica, no Jessica) y corrígela silenciosamente.";

/// `es-ES` — tuteo + castellano.
pub const ES_ES_STYLE_ADDENDUM: &str = "# IDIOMA

Responde estrictamente en español de España (es-ES).

- Tuteo singular (`tú`) + `vosotros` para plural informal. NO uses voseo \
  rioplatense (`vos`, `sos`, `tenés`).
- Vocabulario castellano natural: `vale`, `genial`, `tío`/`tía` en \
  contextos informales, `coger` (sin connotación sexual), `móvil`, \
  `ordenador`, `coche`.
- Cuando el usuario diga su nombre y la transcripción podría haber sido \
  ambigua, prefiere la grafía castellana.";

/// `es-US` — Spanglish-aware.
pub const ES_US_STYLE_ADDENDUM: &str = "# IDIOMA

Responde en español neutral con awareness del contexto bilingüe \
hispano-estadounidense (es-US).

- Tuteo neutral, sin regionalismos fuertes.
- Préstamos del inglés (`email`, `parking`, `shopping`, `lunch`, nombres \
  de marcas) **no se traducen**: úsalos como en inglés. No conviertas \
  `email` en `correo electrónico` ni `parking` en `estacionamiento` salvo \
  que el usuario lo pida.
- Code-switching ligero está bien; mantén la frase principal en español.
- Cuando el usuario diga su nombre y la transcripción podría haber sido \
  ambigua, respeta la preferencia del usuario (Christian o Cristian son \
  ambos válidos en este contexto).";

/// `en-US` / language-only `en`.
pub const EN_US_STYLE_ADDENDUM: &str = "# LANGUAGE

Respond strictly in American English (en-US).

- Standard US spelling: `color`, `organize`, `center`, `program`, \
  `analyze`. NOT `colour`, `organise`, `centre`, `programme`, `analyse`.
- Standard US vocabulary: `elevator`, `truck`, `vacation`, `apartment`. \
  Avoid heavy British-only or AAVE-only slang.";

/// `en-GB` — British spelling and vocab.
pub const EN_GB_STYLE_ADDENDUM: &str = "# LANGUAGE

Respond strictly in British English (en-GB).

- British spelling: `colour`, `organise`, `centre`, `programme`, \
  `analyse`. NOT the American forms.
- British vocabulary: `lift` (not `elevator`), `lorry` (not `truck`), \
  `holiday` (not `vacation`), `flat` (not `apartment`), `boot` (car), \
  `biscuit` (cookie), `chips` (fries), `crisps` (chips).
- Register: neutral / polite. Use `whilst` / `amongst` sparingly.";

/// `en-AU` — Australian.
pub const EN_AU_STYLE_ADDENDUM: &str = "# LANGUAGE

Respond strictly in Australian English (en-AU).

- Standard spelling acceptable; British forms (`colour`, `organise`) \
  are also natural in formal contexts.
- Australian vocabulary acceptable in moderation: `mate`, `arvo` \
  (afternoon), `g'day`, `barbie` (BBQ), `servo` (gas station). Keep \
  formal contexts neutral.";

/// `pt-BR` / language-only `pt`.
pub const PT_BR_STYLE_ADDENDUM: &str = "# IDIOMA

Responda estritamente em português brasileiro (pt-BR).

- `Você` para singular; `vocês` para plural. NÃO use `tu` (raro fora de \
  algumas regiões) salvo se o usuário usar primeiro.
- Vocabulário brasileiro: `legal`, `bacana`, `celular` (não `telemóvel`), \
  `ônibus` (não `autocarro`), `geladeira` (não `frigorífico`), \
  `tomada` (não `ficha`).";

/// `pt-PT` — português europeu.
pub const PT_PT_STYLE_ADDENDUM: &str = "# IDIOMA

Responda estritamente em português europeu (pt-PT).

- `Tu` informal singular; `vocês` plural; `vós` arcaico (evitar). \
  Conjugação à portuguesa (`tens`, `podes`, `queres`).
- Vocabulário português: `fixe` (legal), `telemóvel` (celular), \
  `autocarro` (ônibus), `frigorífico` (geladeira), `pequeno-almoço` \
  (café da manhã), `ficha` (tomada).
- Construções gramaticais europeias (mesóclise / próclise).";

/// French (any region).
pub const FR_STYLE_ADDENDUM: &str = "# LANGUE

Répondez strictement en français.

- Registre poli / vouvoiement par défaut ; passez au tutoiement si \
  l'utilisateur le fait.
- Évitez le slang fortement régional (verlan parisien, joual québécois) \
  sauf si le contexte l'invite explicitement.";

/// Italian.
pub const IT_STYLE_ADDENDUM: &str = "# LINGUA

Rispondi rigorosamente in italiano standard.

- Lei formale per default; passa al tu se l'utente lo fa.
- Evita dialetti regionali; mantieni un registro neutro.";

/// German.
pub const DE_STYLE_ADDENDUM: &str = "# SPRACHE

Antworten Sie ausschließlich auf Hochdeutsch.

- Sie-Form formell standardmäßig; wechseln Sie zum Du, wenn der \
  Benutzer es zuerst tut.
- Vermeiden Sie starke Dialekte (Bayerisch, Schwäbisch, Plattdeutsch) \
  und schweizerische / österreichische Eigenheiten, falls nicht \
  explizit gewünscht.";

/// Japanese.
pub const JA_STYLE_ADDENDUM: &str = "# 言語

日本語で厳密に回答してください。

- デフォルトは丁寧語 (ですます調) 。ユーザーがカジュアルに切り替えた \
  場合のみ追従。
- 強い方言は避けてください (関西弁・東北弁など) ユーザーが望まない \
  限り。";

/// Chinese (Simplified).
pub const ZH_STYLE_ADDENDUM: &str = "# 语言

请严格使用简体中文回复。

- 默认采用标准书面语；如果用户使用口语化中文，则跟随其语域。
- 避免强烈的地方方言 (粤语、闽南语、吴语) 除非用户明确要求。";

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn loc(s: &str) -> Locale {
        Locale::from_str(s).expect("valid locale")
    }

    // ── Voice picker ───────────────────────────────────────────

    #[test]
    fn voice_picker_es_family() {
        assert_eq!(default_voice_for_locale(Some(&loc("es-AR"))), "es-AR-ElenaNeural");
        assert_eq!(default_voice_for_locale(Some(&loc("es-MX"))), "es-MX-DaliaNeural");
        assert_eq!(default_voice_for_locale(Some(&loc("es-ES"))), "es-ES-ElviraNeural");
        assert_eq!(default_voice_for_locale(Some(&loc("es-CO"))), "es-CO-SalomeNeural");
        assert_eq!(default_voice_for_locale(Some(&loc("es-PE"))), "es-PE-CamilaNeural");
        assert_eq!(default_voice_for_locale(Some(&loc("es-CL"))), "es-CL-CatalinaNeural");
        assert_eq!(default_voice_for_locale(Some(&loc("es-US"))), "es-US-PalomaNeural");
        // Language-only `es` falls back to MX.
        assert_eq!(default_voice_for_locale(Some(&loc("es"))), "es-MX-DaliaNeural");
    }

    #[test]
    fn voice_picker_en_family() {
        assert_eq!(default_voice_for_locale(Some(&loc("en-US"))), "en-US-AriaNeural");
        assert_eq!(default_voice_for_locale(Some(&loc("en-GB"))), "en-GB-SoniaNeural");
        assert_eq!(default_voice_for_locale(Some(&loc("en-AU"))), "en-AU-NatashaNeural");
        assert_eq!(default_voice_for_locale(Some(&loc("en-CA"))), "en-CA-ClaraNeural");
        assert_eq!(default_voice_for_locale(Some(&loc("en"))), "en-US-AriaNeural");
    }

    #[test]
    fn voice_picker_pt_fr_it_de_ja_zh() {
        assert_eq!(default_voice_for_locale(Some(&loc("pt-BR"))), "pt-BR-FranciscaNeural");
        assert_eq!(default_voice_for_locale(Some(&loc("pt-PT"))), "pt-PT-RaquelNeural");
        assert_eq!(default_voice_for_locale(Some(&loc("pt"))), "pt-BR-FranciscaNeural");
        assert_eq!(default_voice_for_locale(Some(&loc("fr-FR"))), "fr-FR-DeniseNeural");
        assert_eq!(default_voice_for_locale(Some(&loc("fr-CA"))), "fr-CA-SylvieNeural");
        assert_eq!(default_voice_for_locale(Some(&loc("fr"))), "fr-FR-DeniseNeural");
        assert_eq!(default_voice_for_locale(Some(&loc("it-IT"))), "it-IT-ElsaNeural");
        assert_eq!(default_voice_for_locale(Some(&loc("de-DE"))), "de-DE-KatjaNeural");
        assert_eq!(default_voice_for_locale(Some(&loc("ja-JP"))), "ja-JP-NanamiNeural");
        assert_eq!(default_voice_for_locale(Some(&loc("zh-CN"))), "zh-CN-XiaoxiaoNeural");
    }

    #[test]
    fn voice_picker_none_falls_back_to_default() {
        assert_eq!(default_voice_for_locale(None), DEFAULT_VOICE_ID);
    }

    // ── Voice-mode addendum (BUG-FIX regression guards) ────────

    #[test]
    fn voice_mode_es_ar_uses_voseo() {
        let addendum = voice_mode_addendum(Some(&loc("es-AR")));
        assert!(addendum.contains("Pensá"), "es-AR voice addendum must use voseo (`Pensá`)");
        assert!(addendum.contains("Tenés"), "es-AR voice addendum must use voseo (`Tenés`)");
        assert!(addendum.contains("Marcá"), "es-AR voice addendum must use voseo (`Marcá`)");
    }

    #[test]
    fn voice_mode_es_latam_avoids_voseo() {
        // Regression guard: `language: \"es\"` agents previously inherited
        // the Argentine flavour by accident. The Latam-neutral template
        // must NOT contain voseo verbs.
        let addendum = voice_mode_addendum(Some(&loc("es-MX")));
        assert!(addendum.contains("Piensa"), "es-MX must use tuteo (`Piensa`)");
        assert!(!addendum.contains("Pensá"), "es-MX must NOT use voseo (`Pensá`)");
        assert!(!addendum.contains("Marcá"), "es-MX must NOT use voseo (`Marcá`)");
        assert!(addendum.contains("Tienes"), "es-MX must use tuteo (`Tienes`)");
        // `es` (language-only) routes to the same Latam template.
        let lang_only = voice_mode_addendum(Some(&loc("es")));
        assert_eq!(addendum, lang_only);
    }

    #[test]
    fn voice_mode_es_es_uses_castellano() {
        let addendum = voice_mode_addendum(Some(&loc("es-ES")));
        assert!(addendum.contains("vosotros") || addendum.contains("os preocupe"),
            "es-ES must reference castellano `vosotros`");
        assert!(addendum.contains("vale") || addendum.contains("genial"),
            "es-ES must include castellano vocabulary cues");
    }

    #[test]
    fn voice_mode_none_uses_latam_neutral() {
        let none = voice_mode_addendum(None);
        let latam = voice_mode_addendum(Some(&loc("es-MX")));
        assert_eq!(none, latam, "None falls back to Latam-neutral, not legacy voseo");
    }

    #[test]
    fn voice_mode_en_gb_uses_british_spelling_cue() {
        let addendum = voice_mode_addendum(Some(&loc("en-GB")));
        assert!(addendum.contains("colour") || addendum.contains("organise") || addendum.contains("centre"),
            "en-GB must include British spelling cue");
    }

    // ── Style addendum ─────────────────────────────────────────

    #[test]
    fn style_addendum_none_returns_none() {
        assert!(language_style_addendum(None).is_none());
    }

    #[test]
    fn style_es_ar_affirms_voseo() {
        let s = language_style_addendum(Some(&loc("es-AR"))).unwrap();
        assert!(s.contains("Voseo") || s.contains("voseo"));
        assert!(s.contains("vos"));
    }

    #[test]
    fn style_es_latam_forbids_voseo() {
        let s = language_style_addendum(Some(&loc("es-MX"))).unwrap();
        assert!(s.contains("NO uses voseo"));
        assert!(s.contains("Tuteo"));
    }

    #[test]
    fn style_es_es_mentions_vosotros() {
        let s = language_style_addendum(Some(&loc("es-ES"))).unwrap();
        assert!(s.contains("vosotros"));
    }

    #[test]
    fn style_es_us_mentions_spanglish_loanwords() {
        let s = language_style_addendum(Some(&loc("es-US"))).unwrap();
        assert!(s.contains("email") || s.contains("parking"));
        assert!(s.contains("no se traducen") || s.contains("Préstamos"));
    }

    #[test]
    fn style_en_us_mentions_american_spelling() {
        let s = language_style_addendum(Some(&loc("en-US"))).unwrap();
        assert!(s.contains("color"));
        assert!(s.contains("American"));
    }

    #[test]
    fn style_en_gb_mentions_british_spelling_and_vocab() {
        let s = language_style_addendum(Some(&loc("en-GB"))).unwrap();
        assert!(s.contains("colour"));
        assert!(s.contains("lift") && s.contains("elevator"));
    }

    #[test]
    fn every_supported_locale_has_style_addendum() {
        // Smoke-test the full closed enum surface — every locale
        // the parser accepts must produce Some(_) here.
        let codes = [
            "es-AR", "es-MX", "es-ES", "es-CO", "es-PE", "es-CL", "es-US", "es",
            "en-US", "en-GB", "en-AU", "en-CA", "en",
            "pt-BR", "pt-PT", "pt",
            "fr-FR", "fr-CA", "fr",
            "it-IT", "it",
            "de-DE", "de",
            "ja-JP", "ja",
            "zh-CN", "zh",
        ];
        for code in codes {
            let l = loc(code);
            assert!(
                language_style_addendum(Some(&l)).is_some(),
                "no style addendum for {code}"
            );
            // Voice picker must also resolve every parsed locale.
            let voice = default_voice_for_locale(Some(&l));
            assert!(!voice.is_empty(), "empty voice for {code}");
        }
    }
}
