# Vigilante API Reference

## Tecnologías Utilizadas

- HTTP REST API: Endpoints GET/POST/DELETE para gestión del sistema
- Server-Sent Events (SSE): Streaming de eventos de grabaciones y logs
- MJPEG Streaming: Video en vivo por HTTP (GET/HEAD)
- MP3 Audio Streaming: Audio en tiempo real (GET)
- WebRTC: Señalización disponible; transporte puede no estar habilitado en todas las instalaciones

## Autenticación

Todos los endpoints requieren autenticación:
- Header: `Authorization: Bearer <PROXY_TOKEN>` (recomendado)
 Query parameter: `?token=<PROXY_TOKEN>` solo para endpoints de streaming y solo si está habilitado por configuración (`STREAM_TOKEN_IN_QUERY=1` o `STREAM_MJPEG_TOKEN_IN_QUERY=1`).
 Nota: El servidor prioriza el header `Authorization: Bearer ...`. Si `STREAM_TOKEN_IN_QUERY` está deshabilitado, las peticiones con `?token=` devolverán 401.

## Endpoints

### Sistema
- GET `/api/health` — Healthcheck simple. Respuesta: `{ "status": "ok" }`
 
 Para reproductores como ffplay/ffmpeg que aceptan cabeceras manuales, use el flag `-headers` y recuerde incluir el CRLF al final de la línea de cabecera:
 
 ```bash
 # ffplay
 ffplay -headers "Authorization: Bearer mi_token_seguro\r\n" \
   -fflags nobuffer -flags low_delay -framedrop -probesize 32 -analyzeduration 0 \
   "http://localhost:8080/api/live/mjpeg"
 
 # ffmpeg (descarga corta)
 ffmpeg -headers "Authorization: Bearer mi_token_seguro\r\n" \
   -fflags nobuffer -flags low_delay -i "http://localhost:8080/api/live/mjpeg" -t 5 dump.mkv
 ```
- GET `/api/status` — Estado general del sistema
- GET `/metrics` — Métricas Prometheus (content-type: `text/plain; version=0.0.4`)

### Cámara y Streaming
- GET `/api/live/mjpeg` — Stream MJPEG en vivo (Content-Type: `multipart/x-mixed-replace; boundary=frame`)
- HEAD `/api/live/mjpeg` — Preflight/sondeo rápido (sin cuerpo, mismos headers de stream)
- GET `/api/stream/audio` — Stream de audio MP3 (Content-Type: `audio/mpeg`)
- GET `/api/stream/av` — Stream AV combinado (experimental)

### WebRTC (señalización)
- POST `/api/webrtc/offer`
- POST `/api/webrtc/answer/:client_id`
- POST `/api/webrtc/close/:client_id`

### Control PTZ
- POST `/api/ptz/pan/left` — Mover cámara a la izquierda
- POST `/api/ptz/pan/right` — Mover cámara a la derecha
- POST `/api/ptz/tilt/up` — Inclinar cámara hacia arriba
- POST `/api/ptz/tilt/down` — Inclinar cámara hacia abajo
- POST `/api/ptz/zoom/in` — Acercar zoom
- POST `/api/ptz/zoom/out` — Alejar zoom
- POST `/api/ptz/stop` — Detener todos los movimientos

### Almacenamiento
- GET `/api/recordings/summary` — Resumen de grabaciones por día
- GET `/api/recordings/day/:date` — Lista de grabaciones de un día específico (`YYYY-MM-DD`)
- GET `/api/recordings/stream/*path` — Stream de una grabación existente
- DELETE `/api/recordings/delete/*path` — Eliminar una grabación por ruta relativa (p. ej. `2025-09-28/recording_001.mkv`)
- GET `/api/storage` — Resumen de almacenamiento
- GET `/api/storage/info` — Información detallada de almacenamiento
- GET `/api/system/storage` — Info de almacenamiento del sistema
- GET `/api/storage/stream` — Eventos SSE de grabaciones y almacenamiento

### Logs
- GET `/api/logs/stream` — Streaming de logs en tiempo real (SSE)
- GET `/api/logs/entries/:date` — Entradas de log por fecha (`YYYY-MM-DD`)

## Ejemplos de Uso

### Healthcheck
```bash
curl -H "Authorization: Bearer mi_token_seguro" \
  http://localhost:8080/api/health
```

### MJPEG: HEAD (preflight)
```bash
curl -I -H "Authorization: Bearer mi_token_seguro" \
  http://localhost:8080/api/live/mjpeg
```

### MJPEG: stream en vivo (Header)
```bash
curl -H "Authorization: Bearer mi_token_seguro" \
  http://localhost:8080/api/live/mjpeg
```

### Streaming con token en query (si está habilitado)
```bash
# MJPEG
curl "http://localhost:8080/api/live/mjpeg?token=mi_token_seguro"

# Audio MP3
curl "http://localhost:8080/api/stream/audio?token=mi_token_seguro"
```

### Mover cámara (PTZ)
```bash
curl -X POST -H "Authorization: Bearer mi_token_seguro" \
  http://localhost:8080/api/ptz/pan/left
```

### Obtener resumen de grabaciones
```bash
curl -H "Authorization: Bearer mi_token_seguro" \
  http://localhost:8080/api/recordings/summary
```

Respuesta JSON (200):
```json
[
  { "name": "2025-04-31", "records": 24 },
  { "name": "2025-05-01", "records": 18 }
]
```

### Obtener grabaciones de un día específico
```bash
curl -H "Authorization: Bearer mi_token_seguro" \
  http://localhost:8080/api/recordings/day/2025-09-28
```

Respuesta JSON (200):
```json
[
  {
    "name": "recording_001.mkv",
    "path": "2025-09-28/recording_001.mkv",
    "size": 154857267,
    "last_modified": "2025-09-28T10:30:15.123Z",
    "duration": null,
    "day": "2025-09-28"
  }
]
```

### Stream de una grabación existente
```bash
curl -H "Authorization: Bearer mi_token_seguro" \
  http://localhost:8080/api/recordings/stream/2025-09-28/recording_001.mkv
```

### Eliminar grabación (DELETE)
```bash
curl -X DELETE -H "Authorization: Bearer mi_token_seguro" \
  http://localhost:8080/api/recordings/delete/2025-09-28/recording_001.mkv
```

### Eventos de almacenamiento y grabaciones (SSE)
```bash
curl -H "Authorization: Bearer mi_token_seguro" \
  -H "Accept: text/event-stream" \
  http://localhost:8080/api/storage/stream
```

### Streaming de logs (SSE)
```bash
curl -H "Authorization: Bearer mi_token_seguro" \
  -H "Accept: text/event-stream" \
  http://localhost:8080/api/logs/stream
```

### Obtener estado del sistema
```bash
curl -H "Authorization: Bearer mi_token_seguro" \
  http://localhost:8080/api/status
```

## Respuestas de Error

Los endpoints devuelven códigos HTTP estándar:
- 200 — Éxito
- 401 — No autorizado
- 404 — Recurso no encontrado
- 500 — Error interno del servidor

Ejemplo de error:
```json
{ "error": "Descripción del error" }
```