# Vigilante API Reference

## Tecnologías Utilizadas

- **HTTP REST API**: Endpoints GET/POST/DELETE para gestión del sistema
- **WebSocket (WS)**: Conexión en tiempo real para frames MJPEG y comandos
- **Server-Sent Events (SSE)**: Streaming de eventos de grabaciones y logs
- **MJPEG Streaming**: Video en vivo comprimido (GET)
- **MP3 Audio Streaming**: Audio en tiempo real (GET)

## Autenticación

Todos los endpoints requieren autenticación mediante:
- **Header**: `Authorization: Bearer <PROXY_TOKEN>`
- **Query Parameter**: `?token=<PROXY_TOKEN>` (para streams)

## Endpoints

### Sistema
- `GET /api/status` - Estado general del sistema (HTTP GET)
- `GET /api/metrics` - Métricas de rendimiento Prometheus (HTTP GET)

### Cámara y Streaming
- `GET /api/live/mjpeg` - Stream MJPEG en vivo (HTTP GET)
- `GET /api/live/audio` - Stream de audio MP3 (HTTP GET)

### Control PTZ
- `POST /api/ptz/pan_left` - Mover cámara a la izquierda (HTTP POST)
- `POST /api/ptz/pan_right` - Mover cámara a la derecha (HTTP POST)
- `POST /api/ptz/tilt_up` - Inclinar cámara hacia arriba (HTTP POST)
- `POST /api/ptz/tilt_down` - Inclinar cámara hacia abajo (HTTP POST)
- `POST /api/ptz/zoom_in` - Acercar zoom (HTTP POST)
- `POST /api/ptz/zoom_out` - Alejar zoom (HTTP POST)
- `POST /api/ptz/stop` - Detener todos los movimientos (HTTP POST)

### Almacenamiento
- `GET /api/recordings/summary` - Resumen de grabaciones por día (HTTP GET)
- `GET /api/recordings/day/:date` - Lista detallada de grabaciones de un día específico (HTTP GET)
- `GET /api/recordings/sse` - Eventos SSE de grabaciones (Server-Sent Events)

### WebSocket
- `WS /ws` - Conexión WebSocket para frames MJPEG y comandos (WebSocket)

### Logs
- `GET /api/logs/stream` - Streaming de logs en tiempo real (Server-Sent Events)
- `GET /api/logs/entries/{date}` - Obtener entradas de log por fecha (HTTP GET)

## Ejemplos de Uso

### Ver stream en vivo (HTTP GET)
```bash
curl -H "Authorization: Bearer mi_token_seguro" \
     http://localhost:8080/api/live/mjpeg
```

### Streaming con token en query (HTTP GET)
```bash
# MJPEG
curl "http://localhost:8080/api/live/mjpeg?token=mi_token_seguro"

# Audio
curl "http://localhost:8080/api/live/audio?token=mi_token_seguro"
```

### Mover cámara (HTTP POST)
```bash
curl -X POST \
     -H "Authorization: Bearer mi_token_seguro" \
     http://localhost:8080/api/ptz/pan_left
```

### Obtener resumen de grabaciones (HTTP GET)
```bash
curl -H "Authorization: Bearer mi_token_seguro" \
     http://localhost:8080/api/recordings/summary
```

Respuesta JSON (HTTP 200):
```json
[
  {
    "name": "2025-04-31",
    "records": 24
  },
  {
    "name": "2025-05-01",
    "records": 18
  }
]
```

### Obtener grabaciones de un día específico (HTTP GET)
```bash
curl -H "Authorization: Bearer mi_token_seguro" \
     http://localhost:8080/api/recordings/day/2025-09-28
```

Respuesta JSON (HTTP 200):
```json
[
  {
    "name": "recording_001.mkv",
    "path": "2025-09-28/recording_001.mkv",
    "size": 154857267,
    "last_modified": "2025-09-28T10:30:15.123Z",
    "duration": null,
    "day": "2025-09-28"
  },
  {
    "name": "recording_002.mkv",
    "path": "2025-09-28/recording_002.mkv",
    "size": 145892341,
    "last_modified": "2025-09-28T11:15:22.456Z",
    "duration": null,
    "day": "2025-09-28"
  }
]
```

### Verificar estado del sistema (HTTP GET)
```bash
curl -H "Authorization: Bearer mi_token_seguro" \
     http://localhost:8080/api/status
```

Respuesta JSON (HTTP 200):
```json
{
  "camera_works": true,
  "audio_works": true,
  "recordings_work": true
}
```

### Eventos SSE de grabaciones (Server-Sent Events)
```bash
curl -H "Authorization: Bearer mi_token_seguro" \
     -H "Accept: text/event-stream" \
     http://localhost:8080/api/recordings/sse
```

Respuesta SSE:
```
data: {"type": "recording_started", "path": "2025-09-28/recording_001.mkv"}

data: {"type": "recording_stopped", "path": "2025-09-28/recording_001.mkv"}
```

### Streaming de logs (Server-Sent Events)
```bash
curl -H "Authorization: Bearer mi_token_seguro" \
     -H "Accept: text/event-stream" \
     http://localhost:8080/api/logs/stream
```

### WebSocket para frames en tiempo real
```javascript
const ws = new WebSocket('ws://localhost:8080/ws?token=mi_token_seguro');

ws.onmessage = (event) => {
    const data = JSON.parse(event.data);
    if (data.type === 'frame') {
        // Procesar frame MJPEG
        displayFrame(data.frame);
    }
};
```

### Eliminar grabación (HTTP DELETE)
```bash
curl -X DELETE \
     -H "Authorization: Bearer mi_token_seguro" \
     http://localhost:8080/api/recordings/2025-09-28/recording_001.mkv
```

### Obtener entradas de log por fecha (HTTP GET)
```bash
curl -H "Authorization: Bearer mi_token_seguro" \
     http://localhost:8080/api/logs/entries/2025-09-28
```

Respuesta JSON (HTTP 200):
```json
[
  {
    "timestamp": "2025-09-28T10:30:15.123Z",
    "level": "INFO",
    "message": "Recording started: 2025-09-28/recording_001.mkv"
  },
  {
    "timestamp": "2025-09-28T11:15:22.456Z",
    "level": "INFO",
    "message": "Recording stopped: 2025-09-28/recording_001.mkv"
  }
]
```

## Respuestas de Error

Los endpoints devuelven códigos HTTP estándar:
- `200` - Éxito
- `401` - No autorizado
- `404` - Recurso no encontrado
- `500` - Error interno del servidor

Respuesta de error ejemplo:
```json
{
  "error": "Descripción del error"
}
```

## Respuestas de Error

Los endpoints devuelven códigos HTTP estándar:
- `200` - Éxito
- `401` - No autorizado
- `404` - Recurso no encontrado
- `500` - Error interno del servidor

Respuesta de error ejemplo:
```json
{
  "error": "Descripción del error"
}
```