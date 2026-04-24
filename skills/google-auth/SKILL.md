---
name: Google OAuth
description: Authenticate against the user's own Google account (Gmail, Drive, Calendar, Sheets, etc.) via the installed-application OAuth 2.0 flow and make authenticated HTTP calls.
requires:
  bins: []
  env: []
---

# Google OAuth

Drives the agent's access to the user's Google services — Gmail,
Drive, Calendar, Sheets, Photos, YouTube Data, Tasks, and every other
`*.googleapis.com` endpoint. Backed by 4 tools (`google_auth_start`,
`google_auth_status`, `google_call`, `google_auth_revoke`) that wrap a
real OAuth 2.0 installed-application flow with a loopback callback and
persistent refresh tokens.

## Use when

- "Léeme los últimos 10 emails de Gmail"
- "Crea un evento en mi Calendar para mañana a las 3pm"
- "Busca el doc 'Q4 budget' en mi Drive"
- "Agrega una fila al Sheet de gastos"
- "Borra los videos que subí a YouTube este mes"

## Do not use when

- **Apps de Google que no son del usuario** (Drive de un admin, Gmail
  de otra persona) — cada agente solo accede a la cuenta con la que se
  autorizó.
- **Acciones gratuitas que no requieren cuenta** (búsqueda pública,
  Maps sin cuenta, etc.) — hay tools dedicadas (`ext_wikipedia_search`,
  etc.) que no gastan el rate-limit de Google APIs.
- **Gemini/AI Studio** — ese no va por OAuth, va por la API key directa
  (variable `GEMINI_API_KEY`), distinto universo.

## Canonical workflow

Primer turno donde el user pide algo de Google:

```
1. google_auth_status()
   → { authenticated: false, reason: "no tokens on file" }

2. google_auth_start()
   → { ok: true, url: "https://accounts.google.com/o/oauth2/v2/auth?...",
       redirect_uri: "http://127.0.0.1:8765/callback",
       instructions: "Open this URL..." }

3. Mandas al usuario vía chat:
   "Para que pueda leer tu Gmail, abrí este link y aceptá los permisos:
    <url>. Si estás conectando desde un server remoto, necesitás un SSH
    tunnel: `ssh -L 8765:127.0.0.1:8765 <host>`. Avisame cuando termines."

4. Espera el próximo mensaje del user. NO reintentes tools mientras tanto.

5. Cuando user responda "listo" o algo similar:
   google_auth_status()
   → { authenticated: true, expires_in_secs: 3589, has_refresh: true,
       scopes: [...] }

6. Ya puedes hacer google_call(...) con lo que necesite el user.
```

Todas las llamadas siguientes (incluso en conversaciones futuras días
después) solo necesitan **`google_call`** directo. El refresh_token
persistido renueva el access_token automáticamente — el usuario NO
vuelve a ver un popup de consentimiento hasta que revoque.

## Tool reference

| Tool | Cuándo llamar |
|---|---|
| `google_auth_status` | Antes de la primera operación Google de cada sesión. Diagnóstico rápido — no toca red. |
| `google_auth_start` | Solo si `status.authenticated == false`. Requiere intervención humana (click + consent en browser). No reintenta, no hace polling — espera al siguiente mensaje del user. |
| `google_call` | El workhorse. 99% de tus llamadas. |
| `google_auth_revoke` | Solo si el user explícitamente pide "olvida mi Gmail" / "desconecta Google". Invalida el token en Google. |

## `google_call` — ejemplos por API

### Gmail

```json
{ "method": "GET",
  "url": "https://gmail.googleapis.com/gmail/v1/users/me/messages?maxResults=10" }

{ "method": "GET",
  "url": "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}?format=full" }

{ "method": "POST",
  "url": "https://gmail.googleapis.com/gmail/v1/users/me/messages/send",
  "body": { "raw": "<base64-encoded RFC 2822 message>" } }
```

Scope requerido: `gmail.readonly` (read) o `gmail.modify` (send/trash).

### Calendar

```json
{ "method": "GET",
  "url": "https://www.googleapis.com/calendar/v3/calendars/primary/events?timeMin=2026-04-24T00:00:00Z" }

{ "method": "POST",
  "url": "https://www.googleapis.com/calendar/v3/calendars/primary/events",
  "body": {
    "summary": "Dentist",
    "start": {"dateTime": "2026-04-25T15:00:00-05:00"},
    "end":   {"dateTime": "2026-04-25T16:00:00-05:00"}
  } }
```

Scope: `calendar.readonly` o `calendar.events`.

### Drive

```json
{ "method": "GET",
  "url": "https://www.googleapis.com/drive/v3/files?pageSize=50&q=name contains 'budget'" }

{ "method": "GET",
  "url": "https://www.googleapis.com/drive/v3/files/{fileId}?fields=*" }
```

Scope: `drive.readonly` o `drive`.

### Sheets

```json
{ "method": "GET",
  "url": "https://sheets.googleapis.com/v4/spreadsheets/{id}/values/Sheet1!A1:D100" }

{ "method": "POST",
  "url": "https://sheets.googleapis.com/v4/spreadsheets/{id}/values/Sheet1!A1:append?valueInputOption=USER_ENTERED",
  "body": { "values": [["2026-04-24", "coffee", 5.50, "food"]] } }
```

Scope: `spreadsheets.readonly` o `spreadsheets`.

## Scopes commonly requested

Configúralos en `agents.yaml` → `google_auth.scopes`:

| Corto | URL canónica | Alcance |
|---|---|---|
| `userinfo.email` | `https://www.googleapis.com/auth/userinfo.email` | email del user |
| `userinfo.profile` | `https://www.googleapis.com/auth/userinfo.profile` | nombre + foto |
| `gmail.readonly` | `/auth/gmail.readonly` | leer mails (no marcar, no enviar) |
| `gmail.send` | `/auth/gmail.send` | enviar, sin leer |
| `gmail.modify` | `/auth/gmail.modify` | todo excepto borrar permanente |
| `calendar.readonly` | `/auth/calendar.readonly` | ver agenda |
| `calendar.events` | `/auth/calendar.events` | crear/editar eventos |
| `drive.readonly` | `/auth/drive.readonly` | leer Drive |
| `drive.file` | `/auth/drive.file` | solo archivos creados por esta app |
| `drive` | `/auth/drive` | acceso total (peligroso) |
| `spreadsheets.readonly` | `/auth/spreadsheets.readonly` | leer Sheets |
| `spreadsheets` | `/auth/spreadsheets` | leer + escribir |
| `youtube.readonly` | `/auth/youtube.readonly` | ver info de tu canal |

## Failure modes

- `401 Unauthorized` → token murió. Intenta `google_auth_status`; si
  `authenticated: false`, llama `google_auth_start` y pide re-consent.
- `403 Forbidden` → el scope no fue otorgado. Updatea
  `google_auth.scopes` en config, corre `google_auth_revoke` + de nuevo
  `google_auth_start` (Google no mezcla scopes viejos con nuevos).
- `refresh_token` "invalid_grant" → user revocó acceso, o la app está
  en "Testing" y pasaron 7 días. `google_auth_start` para refrescar.
- Puerto 8765 ocupado → `google_auth.redirect_port` a otro libre en
  `agents.yaml`, también actualizá el "Authorized redirect URI" en
  Google Cloud Console.

## Gotchas

- **No compartas refresh_token entre agentes.** Cada agente tiene su
  propio profile y archivo de tokens. El agente "kate" logueado no
  autoriza al agente "alex" — eso es por diseño.
- **Los scopes se granulan en el primer consent.** Si después querés
  añadir uno nuevo, revoke + re-start. Google no expande scopes sobre
  un refresh_token existente.
- **App en "Testing" mode**: refresh_tokens expiran a los 7 días. Para
  prod, publicá la app en Google Cloud Console (verificación opcional
  para scopes sensibles como `gmail.modify`).
- **Rate limits son por proyecto Cloud, no por usuario**: si varios
  agentes usan el mismo `client_id` y explotan Gmail, comparten la
  quota. Crear un proyecto por agente es caro pero aisla.
