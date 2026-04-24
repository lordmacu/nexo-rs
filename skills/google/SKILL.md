---
name: Google
description: Gmail, Calendar, Tasks, Drive, Contacts (People) y Photos via Google APIs con OAuth user refresh-token. 32 tools, resolve-by-name contacts.
requires:
  bins: []
  env:
    - GOOGLE_CLIENT_ID
    - GOOGLE_CLIENT_SECRET
    - GOOGLE_REFRESH_TOKEN
---

# Google

Use este skill cuando kate necesite consultar o accionar sobre servicios
Google del usuario: correo, calendario, tareas, archivos en Drive.
La extension `google` implementa las llamadas a las APIs REST; el OAuth
refresh_token se maneja en env vars.

## Use when

- Leer, buscar o resumir emails
- Listar/crear/mover eventos de calendar
- Consultar o agregar items a Tasks
- Listar/descargar/subir archivos en Drive

## Do not use when

- Querés ejecutar acciones de write sin que el operador haya activado el
  flag correspondiente (ver abajo) — la tool devolverá `-32043`
- Quieres listar todas las APIs de Google que existen — esta extension
  cubre 4 servicios (Gmail/Calendar/Tasks/Drive); el resto no está expuesto

## Tool surface

**21 tools** agrupadas por servicio. Reads sin restricción; writes gated.

### Status
- `status` — credential presence, endpoints, write-flag state

### Gmail (5 tools)
- `gmail_list(query?, label_ids?, max_results?, include_spam_trash?, page_token?)` — metadata (id + thread_id)
- `gmail_read(id, format?)` — mensaje completo (headers, body_text decoded, labels)
- `gmail_search(query, max_results?)` — alias de list con query obligatoria
- `gmail_send(to, subject, body)` — **requires `GOOGLE_ALLOW_SEND=true`**
- `gmail_modify_labels(id, add_labels?, remove_labels?)` — **gated** (mark_read, archive, trash)

### Calendar (5 tools)
- `calendar_list_calendars`
- `calendar_list_events(calendar_id?, time_min?, time_max?, q?, max_results?, single_events?, order_by?)`
- `calendar_create_event(calendar_id?, summary, description?, location?, start, end, time_zone?, attendees?)` — **requires `GOOGLE_ALLOW_CALENDAR_WRITE=true`**
- `calendar_update_event(calendar_id?, event_id, patch)` — **gated**
- `calendar_delete_event(calendar_id?, event_id)` — **gated**

### Tasks (5 tools)
- `tasks_list_lists(max_results?)`
- `tasks_list_tasks(list_id, show_completed?, show_hidden?, max_results?)`
- `tasks_add(list_id, title, notes?, due?)` — **requires `GOOGLE_ALLOW_TASKS_WRITE=true`**
- `tasks_complete(list_id, task_id)` — **gated**
- `tasks_delete(list_id, task_id)` — **gated**

### Drive (6 tools)
- `drive_list(q?, page_size?, fields?, page_token?, spaces?)`
- `drive_get(id, fields?)`
- `drive_download(id, output_path)` — escribe a disco; `output_path` debe estar bajo `GOOGLE_DRIVE_SANDBOX_ROOT`
- `drive_upload(source_path, name?, parent_id?, mime_type?)` — **requires `GOOGLE_ALLOW_DRIVE_WRITE=true`**; source bajo sandbox
- `drive_create_folder(name, parent_id?)` — **gated**
- `drive_delete(id)` — **gated**

## Execution guidance

### Leer email
1. `gmail_search query:"is:unread newer_than:1d" max_results:10` → ids
2. `gmail_read id:<id>` por cada uno que importe → body_text + headers
3. Opcional: `gmail_modify_labels` para marcar como leído (gated)

### Resumen del día
1. `calendar_list_events time_min:<hoy 00:00Z> time_max:<mañana 00:00Z>` → agenda
2. `tasks_list_tasks list_id:@default show_completed:false` → pendientes
3. `gmail_search query:"is:unread"` → urgente

### Procesar un PDF de Drive
1. `drive_list q:"mimeType='application/pdf' and name contains 'factura'"` → id
2. `drive_download id:<id> output_path:/sandbox/factura.pdf` → local
3. `pdf-extract.extract_text path:/sandbox/factura.pdf` → texto
4. `summarize.summarize_text text:<...>` → resumen

### Anti-patterns

- **No exponer body_text del email entero** al usuario en chat si contiene
  PII de terceros; preferí citar solo lo relevante.
- **No loopees tasks_add** sin confirmar; cada call crea un row persistente.
- **No descargues archivos** que no caben en memoria (hard cap ~50 MB en
  reqwest blocking body); pasá por storage si necesitás más.

## OAuth scopes requeridos

El `GOOGLE_REFRESH_TOKEN` debe haberse generado con los scopes necesarios.
Setup recomendado (uso completo):

```
https://www.googleapis.com/auth/gmail.readonly
https://www.googleapis.com/auth/gmail.send
https://www.googleapis.com/auth/gmail.modify
https://www.googleapis.com/auth/calendar.readonly
https://www.googleapis.com/auth/calendar.events
https://www.googleapis.com/auth/tasks
https://www.googleapis.com/auth/drive
```

Si tu token sólo tiene readonly, las tools de write funcionan estructuralmente
(pasan el env flag) pero Google responde con `403 insufficient permissions`
→ surface como `-32012` Forbidden.

## Errors

| Code | Qué significa |
|------|----------------|
| -32011 | unauthorized / refresh failed / scope missing para readonly |
| -32012 | forbidden (scope missing para writes) |
| -32001 | not found (id inexistente) |
| -32013 | rate limited (con retry_after_secs) |
| -32043 | write denied — setea el env flag correspondiente |
| -32602 | bad input (path outside sandbox, URL malformado, enum inválido) |
| -32003 / -32005 / -32004 | transport / timeout / circuit open |
